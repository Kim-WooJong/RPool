//! One drive-history request (CLI, mount request file, GUI) and how it runs
//! on a loaded history: listing/preview read only; restore/rollback apply
//! actions to an open drive; purge publishes a mark.
use super::graph::History;
use super::model::{Retention, HISTORY_VERSION};
use super::restore::Action;
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
/// A drive-history operation; serialized (tag `op`) into mount request files
/// and built by `command::op` or the GUI `api`.
pub(crate) enum Op {
    /// List the trash.
    TrashList,
    /// Restore trash entries.
    TrashRestore {
        /// Trash entry ids to restore.
        ids: Vec<String>,
        /// New path for a single restored entry.
        to: Option<String>,
        /// Destination folder for every entry (names kept).
        into: Option<String>,
    },
    /// `ids`, or every expired entry (`expired`), or the whole trash (`all`).
    /// Without `confirm` only previews.
    TrashPurge {
        /// Explicit trash entry ids.
        ids: Vec<String>,
        /// Every expired entry.
        expired: bool,
        /// Empty the whole trash.
        all: bool,
        /// Publish the mark; `false` previews.
        confirm: bool,
    },
    /// List the versions of one file.
    VersionsList {
        /// Drive path of the file.
        path: String,
    },
    /// Restore one version of a file.
    VersionsRestore {
        /// Drive path of the file.
        path: String,
        /// Revision id to restore.
        id: String,
        /// Restore beside the file instead of replacing it.
        as_copy: bool,
    },
    /// Return `path` (`/` = whole drive) to its state at `at`.
    Rollback {
        /// Drive folder or file to roll back.
        path: String,
        /// Target time (unix seconds).
        at: u64,
        /// Apply; `false` previews.
        confirm: bool,
    },
    /// Physical cleanup of unreferenced drive data (`cleanup`).
    Cleanup(super::cleanup::Request),
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
    /// May run for a long time (uploads, deletions): a mount request waits longer.
    pub(crate) fn long_running(&self) -> bool {
        self.writes_drive() || matches!(self, Self::Cleanup(_))
    }
    /// Publishes a purge mark (no drive write).
    pub(crate) fn marks(&self) -> bool {
        matches!(self, Self::TrashPurge { confirm: true, .. })
    }
}

/// `trash restore` / `versions restore` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RestoreReport {
    /// Always `HISTORY_VERSION`.
    pub version: u32,
    /// Pool name.
    pub pool: String,
    /// Actions carried out (new revisions or deletions).
    pub changes: Vec<Action>,
    /// Every change reached the cloud.
    pub published: bool,
    /// Extra explanations (e.g. publish deferred).
    pub notes: Vec<String>,
}

/// `trash purge|empty` result (preview unless `applied`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PurgeReport {
    /// Always `HISTORY_VERSION`.
    pub version: u32,
    /// Pool name.
    pub pool: String,
    /// Trash entry ids selected for purging.
    pub ids: Vec<String>,
    /// Whether the purge mark was published.
    pub applied: bool,
    /// Bytes no kept revision references any more after this purge (what
    /// a physical cleanup could reclaim; nothing is deleted by the purge).
    pub eligible_bytes: u64,
    /// Explanations for the user.
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
        Op::Cleanup(_) => bail!("cleanup runs through dispatch"),
    })
}

/// Select the trash entries to purge and compute the bytes no kept revision
/// references afterwards. Used for previews (`read`) and by `purge`.
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
    let notes = vec!["purged files are hidden on every PC; their data is deleted later by the drive cleanup (`rpool drive cleanup`, automatic in mounts) after its grace period".into()];
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
        let mut h = history(vec![
            ("d1", rev("d.txt", &[], Some("gone"), "pc", Some(1))),
            ("d2", rev("d.txt", &["d1"], None, "pc", Some(2))),
        ]);
        let d2 = h.id("d2");
        let r = Retention::default();
        let op = Op::TrashPurge {
            ids: vec![d2.clone()],
            expired: false,
            all: false,
            confirm: false,
        };
        let preview: PurgeReport =
            serde_json::from_value(read(&op, &h, "p", &r, 10).unwrap()).unwrap();
        assert_eq!((preview.ids.clone(), preview.applied), (vec![d2], false));
        let store = super::super::marks::fake::Store::default();
        let stores: Vec<&dyn super::super::marks::MarkStore> = vec![&store];
        let done: PurgeReport =
            serde_json::from_value(purge(&op, &stores, &h, "p", &r, "pc", 10).unwrap()).unwrap();
        assert!(done.applied);
        h.purged = super::super::marks::purged(&stores).unwrap();
        assert!(super::super::trash::list(&h, &r, 10).unwrap().is_empty());
    }
}
