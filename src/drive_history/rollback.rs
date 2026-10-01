//! Rollback of a folder (or the whole drive) to time T: make the visible
//! files under the scope what they were at T. Planned against the history's
//! own projection at T and now:
//!
//! - visible at T and now with other bytes: `Revert` (old bytes come back);
//! - visible at T, gone now: `Undelete`;
//! - visible now, not at T: `Remove` (moved to the trash, not destroyed).
//!
//! Applying publishes these as new revisions, so the rollback is itself
//! history: rolling back to a time just before it undoes it.
use super::graph::{display_path, in_scope, History};
use super::model::{ChangeAction, RollbackChange, RollbackPlan, HISTORY_VERSION};
use super::restore::Action;
use crate::prelude::*;

pub(crate) fn plan(history: &History, pool: &str, scope: &str, at: u64) -> Result<RollbackPlan> {
    let then = history.view_at(Some(at))?;
    let now = history.view_at(None)?;
    let size = |id: &str| history.revs[id].content.as_ref().map_or(0, |c| c.size);
    let hash = |id: &str| history.revs[id].content.as_ref().map(|c| c.hash.clone());
    let restorable = |id: &str| {
        history.revs[id]
            .content
            .as_ref()
            .is_some_and(|c| c.restorable)
            && history.payloads.contains_key(id)
    };
    let paths: BTreeSet<&String> = then.keys().chain(now.keys()).collect();
    let mut changes = Vec::new();
    let mut skipped = Vec::new();
    for path in paths.into_iter().filter(|p| in_scope(p, scope)) {
        let (action, revision) = match (then.get(path), now.get(path)) {
            (Some(old), Some(new)) if hash(old) != hash(new) => (ChangeAction::Revert, old),
            (Some(old), None) => (ChangeAction::Undelete, old),
            (None, Some(new)) => (ChangeAction::Remove, new),
            _ => continue,
        };
        if action != ChangeAction::Remove && !restorable(revision) {
            skipped.push((
                display_path(path),
                "data of that version is no longer available (expired or purged)".into(),
            ));
            continue;
        }
        changes.push(RollbackChange {
            path: display_path(path),
            action,
            revision: revision.clone(),
            size: size(revision),
        });
    }
    Ok(RollbackPlan {
        version: HISTORY_VERSION,
        pool: pool.into(),
        scope: display_path(scope),
        at_unix: at,
        changes,
        skipped,
        applied: false,
    })
}

/// Drive actions of a plan.
pub(crate) fn actions(plan: &RollbackPlan) -> Result<Vec<Action>> {
    let mut actions = plan
        .changes
        .iter()
        .map(|change| {
            let path = super::graph::namespace_path(&change.path)?;
            Ok(match change.action {
                ChangeAction::Remove => Action::Delete {
                    path,
                    rev: change.revision.clone(),
                },
                ChangeAction::Revert | ChangeAction::Undelete => Action::Put {
                    path,
                    rev: change.revision.clone(),
                    from: None,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    super::restore::order(&mut actions);
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev};
    use super::super::graph::{Payload, Rev};
    use super::*;

    pub(crate) fn with_payloads(mut h: History) -> History {
        let manifest = Manifest {
            version: 2,
            archive_id: "x".into(),
            original_name: "x".into(),
            original_size: 0,
            shard_size: 1,
            created_unix: 0,
            content_root_blake3: String::new(),
            coding: None,
            shards: vec![],
        };
        for (id, rev) in &h.revs {
            if rev.content.is_some() {
                h.payloads
                    .insert(id.clone(), Payload::Manifest(manifest.clone()));
            }
        }
        h
    }

    fn base() -> Vec<(&'static str, Rev)> {
        vec![
            ("a1", rev("A", &[], Some("a-old"), "pc", Some(10))),
            ("a2", rev("A", &["a1"], Some("a-new"), "pc", Some(30))),
            ("b1", rev("B", &[], Some("b"), "pc", Some(10))),
            ("b2", rev("B", &["b1"], None, "pc", Some(30))),
            ("c1", rev("C", &[], Some("c"), "pc", Some(30))),
            ("d1", rev("D", &[], Some("same"), "pc", Some(10))),
            ("e1", rev("E", &[], Some("outside"), "pc", Some(30))),
        ]
    }
    const NAMES: &[(&str, &str)] = &[
        ("A", "Docs/a.txt"),
        ("B", "Docs/b.txt"),
        ("C", "Docs/c.txt"),
        ("D", "Docs/d.txt"),
        ("E", "Other/e.txt"),
    ];

    #[test]
    fn preview_reverts_undeletes_and_removes_within_scope() {
        let h = with_payloads(history(base(), NAMES));
        let plan = plan(&h, "p", "Docs", 20).unwrap();
        let summary: Vec<_> = plan
            .changes
            .iter()
            .map(|c| (c.path.as_str(), c.action, c.revision.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("/Docs/a.txt", ChangeAction::Revert, "a1"),
                ("/Docs/b.txt", ChangeAction::Undelete, "b1"),
                ("/Docs/c.txt", ChangeAction::Remove, "c1"),
            ]
        );
        assert!(!plan.applied && plan.scope == "/Docs" && plan.skipped.is_empty());
        let whole = super::plan(&h, "p", "", 20).unwrap();
        assert_eq!(whole.changes.len(), 4);
        // Without data the change is skipped, never planned.
        let h = history(base(), NAMES);
        let plan = super::plan(&h, "p", "Docs", 20).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.skipped.len(), 2);
    }

    #[test]
    fn rollback_of_rollback_restores_the_state_before_it() {
        // State after applying the rollback above at time 40.
        let mut revs = base();
        revs.extend([
            ("a3", rev("A", &["a2"], Some("a-old"), "rb", Some(40))),
            ("b3", rev("B", &["b2"], Some("b"), "rb", Some(40))),
            ("c2", rev("C", &["c1"], None, "rb", Some(40))),
        ]);
        let h = with_payloads(history(revs, NAMES));
        assert!(plan(&h, "p", "Docs", 45).unwrap().changes.is_empty());
        // Back to time 35 (after the edits, before the rollback).
        let undo = plan(&h, "p", "Docs", 35).unwrap();
        let summary: Vec<_> = undo
            .changes
            .iter()
            .map(|c| (c.path.as_str(), c.action, c.revision.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("/Docs/a.txt", ChangeAction::Revert, "a2"),
                ("/Docs/b.txt", ChangeAction::Remove, "b3"),
                ("/Docs/c.txt", ChangeAction::Undelete, "c1"),
            ]
        );
        let actions = actions(&undo).unwrap();
        assert!(matches!(&actions[0], Action::Delete { path, .. } if path == "Docs/b.txt"));
    }
}
