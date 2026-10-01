//! One drive-history request (CLI, mount request file, GUI) and how it runs
//! on a loaded history: listing/preview read only; restore/rollback apply
//! actions to an open drive; purge publishes a mark.
use super::graph::History;
use super::model::{Retention, HISTORY_VERSION};
use super::restore::Action;
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Op {
    TrashList,
    TrashRestore {
        ids: Vec<String>,
        to: Option<String>,
        /// Destination folder for every entry (names kept).
        into: Option<String>,
    },
    /// `ids`, or every expired entry (`expired`), or the whole trash (`all`).
    /// Without `confirm` only previews.
    TrashPurge {
        ids: Vec<String>,
        expired: bool,
        all: bool,
        confirm: bool,
    },
    VersionsList {
        path: String,
    },
    VersionsRestore {
        path: String,
        id: String,
        as_copy: bool,
    },
    Rollback {
        path: String,
        at: u64,
        confirm: bool,
    },
}
impl Op {
    /// Changes the drive (needs an open drive, never a read-only listing).
    pub(crate) fn writes_drive(&self) -> bool {
        matches!(
            self,
            Self::TrashRestore { .. }
                | Self::VersionsRestore { .. }
                | Self::Rollback { confirm: true, .. }
        )
    }
    /// Publishes a purge mark (no drive write).
    pub(crate) fn marks(&self) -> bool {
        matches!(self, Self::TrashPurge { confirm: true, .. })
    }
}

/// `trash restore` / `versions restore` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RestoreReport {
    pub version: u32,
    pub pool: String,
    pub changes: Vec<Action>,
    pub published: bool,
    pub notes: Vec<String>,
}

/// `trash purge|empty` result (preview unless `applied`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PurgeReport {
    pub version: u32,
    pub pool: String,
    pub ids: Vec<String>,
    pub applied: bool,
    /// Bytes no kept revision references any more after this purge (what
    /// a physical cleanup could reclaim; nothing is deleted by the purge).
    pub eligible_bytes: u64,
    pub notes: Vec<String>,
}

/// Read-only ops (listing and previews, including a purge preview).
pub(crate) fn read(
    op: &Op,
    history: &History,
    pool: &str,
    retention: &Retention,
    now: u64,
) -> Result<Value> {
    Ok(match op {
        Op::TrashList => serde_json::to_value(super::trash::list(history, retention, now)?)?,
        Op::VersionsList { path } => serde_json::to_value(super::versions::list(
            history,
            &super::graph::namespace_path(path)?,
        )?)?,
        Op::Rollback { path, at, .. } => serde_json::to_value(super::rollback::plan(
            history,
            pool,
            &super::graph::namespace_path(path)?,
            *at,
        )?)?,
        Op::TrashPurge {
            ids, expired, all, ..
        } => serde_json::to_value(purge_report(
            history, pool, retention, now, ids, *expired, *all,
        )?)?,
        Op::TrashRestore { .. } | Op::VersionsRestore { .. } => {
            bail!("restore needs an open drive")
        }
    })
}

pub(crate) fn purge_report(
    history: &History,
    pool: &str,
    retention: &Retention,
    now: u64,
    ids: &[String],
    expired: bool,
    all: bool,
) -> Result<PurgeReport> {
    let selected = super::trash::purge_selection(history, retention, now, ids, expired, all)?;
    let keep = super::retention::protected(history, retention, now, &|id| {
        history.revs.get(id).and_then(|r| r.time)
    });
    let freed: BTreeSet<String> = selected
        .iter()
        .filter_map(|id| history.last_content(id))
        .collect();
    let keep: BTreeSet<String> = keep.difference(&freed).cloned().collect();
    let mut notes = Vec::new();
    if history.mode == "v6" {
        notes.push("v6 keeps all data; purged files are hidden on every PC and their data becomes eligible for a future guarded cleanup (nothing is deleted now)".into());
    } else {
        notes.push("v7: retention stops protecting the purged data; snapshot GC reclaims it unless the pool's history limit still keeps it".into());
    }
    Ok(PurgeReport {
        version: HISTORY_VERSION,
        pool: pool.into(),
        ids: selected.into_iter().collect(),
        applied: false,
        eligible_bytes: super::retention::eligible_bytes(history, &keep)?,
        notes,
    })
}

/// Actions of a drive-writing op.
pub(crate) fn actions(
    op: &Op,
    history: &History,
    retention: &Retention,
    now: u64,
    pool: &str,
) -> Result<(Vec<Action>, Option<super::model::RollbackPlan>)> {
    Ok(match op {
        Op::TrashRestore { ids, to, into } => {
            if ids.is_empty() {
                bail!("choose --id ID...");
            }
            let files = super::trash::files(history, retention, now)?;
            (
                super::trash::restore_actions(
                    history,
                    &files,
                    ids,
                    to.as_deref(),
                    into.as_deref(),
                )?,
                None,
            )
        }
        Op::VersionsRestore { path, id, as_copy } => (
            vec![super::versions::restore_action(
                history,
                &super::graph::namespace_path(path)?,
                id,
                *as_copy,
            )?],
            None,
        ),
        Op::Rollback { path, at, .. } => {
            let plan =
                super::rollback::plan(history, pool, &super::graph::namespace_path(path)?, *at)?;
            (super::rollback::actions(&plan)?, Some(plan))
        }
        _ => bail!("not a drive change"),
    })
}

/// Runs a drive-writing op on `target`.
pub(crate) fn write(
    op: &Op,
    target: &dyn super::apply::Target,
    history: &History,
    pool: &str,
    retention: &Retention,
    now: u64,
) -> Result<Value> {
    let (actions, plan) = actions(op, history, retention, now, pool)?;
    let applied = super::apply::apply(target, history, &actions)?;
    Ok(match plan {
        Some(mut plan) => {
            plan.applied = true;
            for note in &applied.notes {
                eprintln!("{note}");
            }
            serde_json::to_value(plan)?
        }
        None => serde_json::to_value(RestoreReport {
            version: HISTORY_VERSION,
            pool: pool.into(),
            changes: applied.changes,
            published: applied.published,
            notes: applied.notes,
        })?,
    })
}

/// Publishes the purge mark of a confirmed `TrashPurge`.
pub(crate) fn purge(
    op: &Op,
    stores: &[&dyn super::marks::MarkStore],
    history: &History,
    pool: &str,
    retention: &Retention,
    worker: &str,
    now: u64,
) -> Result<Value> {
    let Op::TrashPurge {
        ids, expired, all, ..
    } = op
    else {
        bail!("not a purge");
    };
    let mut report = purge_report(history, pool, retention, now, ids, *expired, *all)?;
    if !report.ids.is_empty() {
        let mark = super::marks::Mark::purge(report.ids.iter().cloned().collect(), worker, now);
        super::marks::publish(stores, &mark)?;
    }
    report.applied = true;
    Ok(serde_json::to_value(report)?)
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev};
    use super::*;

    #[test]
    fn ops_serialize_for_mount_requests_and_classify() {
        let op = Op::Rollback {
            path: "/Docs".into(),
            at: 5,
            confirm: true,
        };
        let text = serde_json::to_string(&op).unwrap();
        assert_eq!(
            text,
            r#"{"op":"rollback","path":"/Docs","at":5,"confirm":true}"#
        );
        assert_eq!(serde_json::from_str::<Op>(&text).unwrap(), op);
        assert!(op.writes_drive());
        assert!(!Op::TrashList.writes_drive());
        assert!(Op::TrashPurge {
            ids: vec![],
            expired: true,
            all: false,
            confirm: true
        }
        .marks());
    }

    #[test]
    fn purge_previews_then_publishes_a_mark_that_hides_the_entry() {
        let mut h = history(
            vec![
                ("d1", rev("D", &[], Some("gone"), "pc", Some(1))),
                ("d2", rev("D", &["d1"], None, "pc", Some(2))),
            ],
            &[("D", "d.txt")],
        );
        let r = Retention::default();
        let op = Op::TrashPurge {
            ids: vec!["d2".into()],
            expired: false,
            all: false,
            confirm: false,
        };
        let preview: PurgeReport =
            serde_json::from_value(read(&op, &h, "p", &r, 10).unwrap()).unwrap();
        assert_eq!(
            (preview.ids.clone(), preview.applied),
            (vec!["d2".to_string()], false)
        );
        let store = super::super::marks::fake::Store::default();
        let stores: Vec<&dyn super::super::marks::MarkStore> = vec![&store];
        let done: PurgeReport =
            serde_json::from_value(purge(&op, &stores, &h, "p", &r, "pc", 10).unwrap()).unwrap();
        assert!(done.applied);
        h.purged = super::super::marks::purged(&stores).unwrap();
        assert!(super::super::trash::list(&h, &r, 10).unwrap().is_empty());
    }
}
