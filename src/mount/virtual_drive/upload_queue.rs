//! Which pending intent may upload next while others are still running.
//!
//! `Namespace::pending` is in acknowledgement order. An intent may start only
//! when no EARLIER, still pending intent:
//! - touches an overlapping path (same path or an ancestor/descendant,
//!   case-folded like `shared_model::overlaps`, on `path` or `event_path`):
//!   per-path order and the namespace's case/file-directory collision checks
//!   then see the same sequence as a strictly serial upload;
//! - is its `depends_on` target (commit requires that receipt);
//! - exists at all when the candidate is a deletion (no spool). Deletions
//!   upload nothing; keeping them behind earlier writes preserves the MOVE
//!   order "destination before source deletion" across different paths.
//!
//! Intents that are running, waiting for a retry or failed in this round are
//! skipped but still block what comes after them.

use super::Intent;
use std::collections::HashSet;

#[derive(Default)]
struct Earlier {
    ids: HashSet<String>,
    /// Folded paths of earlier pending intents.
    exact: HashSet<String>,
    /// Folded proper ancestors of those paths.
    ancestors: HashSet<String>,
    any: bool,
}
impl Earlier {
    fn keys(intent: &Intent) -> [String; 2] {
        [intent.path.to_lowercase(), intent.event_path.to_lowercase()]
    }
    fn proper_ancestors(path: &str) -> impl Iterator<Item = &str> {
        path.match_indices('/').map(move |(at, _)| &path[..at])
    }
    fn blocks(&self, intent: &Intent) -> bool {
        if !self.any {
            return false;
        }
        if intent.spool.is_none() {
            return true;
        }
        if intent
            .depends_on
            .as_ref()
            .is_some_and(|dep| self.ids.contains(dep))
        {
            return true;
        }
        Self::keys(intent).iter().any(|key| {
            self.exact.contains(key)
                || self.ancestors.contains(key)
                || Self::proper_ancestors(key).any(|a| self.exact.contains(a))
        })
    }
    fn add(&mut self, intent: &Intent) {
        self.any = true;
        self.ids.insert(intent.id.clone());
        for key in Self::keys(intent) {
            for ancestor in Self::proper_ancestors(&key) {
                self.ancestors.insert(ancestor.to_owned());
            }
            self.exact.insert(key);
        }
    }
}

/// Index of the first pending intent that may start now; `skip` names intents
/// that must not start (running, backing off, failed this round).
pub(super) fn next_ready(pending: &[Intent], skip: impl Fn(&Intent) -> bool) -> Option<usize> {
    let mut earlier = Earlier::default();
    for (index, intent) in pending.iter().enumerate() {
        if !earlier.blocks(intent) && !skip(intent) {
            return Some(index);
        }
        earlier.add(intent);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(id: &str, path: &str) -> Intent {
        Intent {
            id: id.into(),
            path: path.into(),
            event_path: path.into(),
            parents: vec![],
            spool: Some(id.into()),
            size: 1,
            hash: String::new(),
            depends_on: None,
        }
    }
    fn ready(pending: &[Intent], skip: &[&str]) -> Option<String> {
        next_ready(pending, |i| skip.contains(&i.id.as_str())).map(|i| pending[i].id.clone())
    }

    #[test]
    fn unrelated_paths_run_concurrently_and_one_path_stays_in_order() {
        let pending = [
            write("a1", "dir/a"),
            write("b1", "dir/b"),
            write("a2", "dir/a"),
            write("c1", "c"),
        ];
        assert_eq!(ready(&pending, &[]).as_deref(), Some("a1"));
        assert_eq!(ready(&pending, &["a1"]).as_deref(), Some("b1"));
        // a2 waits for a1 even while a1 is running or failing.
        assert_eq!(ready(&pending, &["a1", "b1"]).as_deref(), Some("c1"));
        assert_eq!(ready(&pending, &["a1", "b1", "c1"]), None);
        // Once a1 committed (left `pending`), a2 may start.
        assert_eq!(ready(&pending[1..], &["b1"]).as_deref(), Some("a2"));
    }

    #[test]
    fn case_folded_and_ancestor_paths_conflict() {
        let pending = [
            write("x", "Doc"),
            write("y", "doc"),
            write("z", "DOC/inner"),
        ];
        assert_eq!(ready(&pending, &["x"]), None);
        let pending = [write("x", "a/b/c"), write("y", "A/B")];
        assert_eq!(ready(&pending, &["x"]), None);
        let mut moved = write("m", "new");
        moved.event_path = "old".into();
        let pending = [write("o", "old"), moved];
        assert_eq!(ready(&pending, &["o"]), None, "event path conflicts too");
    }

    #[test]
    fn dependencies_and_deletions_wait_for_earlier_intents() {
        let mut dependent = write("d", "elsewhere");
        dependent.depends_on = Some("w".into());
        assert_eq!(ready(&[write("w", "a"), dependent], &["w"]), None);
        let mut deletion = write("del", "source");
        deletion.spool = None;
        // MOVE: the destination copy uploads before the source deletion.
        let pending = [write("dest", "destination"), deletion.clone()];
        assert_eq!(ready(&pending, &["dest"]), None);
        assert_eq!(ready(&[deletion], &[]).as_deref(), Some("del"));
    }
}
