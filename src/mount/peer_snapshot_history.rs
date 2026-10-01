//! v7 snapshot state for drive history (trash, versions, rollback), and the
//! retention guard of snapshot GC.
//!
//! GC guard: a retired snapshot's payload objects are deleted once a
//! successor dominates it (`sync_snapshots`). Retention (`drive_history::
//! retention::protected`) may still need some of those bytes (a trash entry
//! within its trash days, a version within keep-versions/version-days that
//! the protocol's `history_limit` no longer retains). Such a snapshot is not
//! journaled for GC yet; it is re-evaluated on every sync and collected once
//! retention no longer needs it. Expiry uses the time this workspace first
//! saw each record (`drive-history-seen.json`), so a fresh workspace protects
//! for a full retention period. Only snapshots whose GC has not started are
//! deferred; a started journal always resumes.
use super::model::{self, Snapshot};
use super::State;
use crate::mount::virtual_drive::VirtualDrive;
use crate::prelude::*;

#[derive(Debug, Clone)]
pub(crate) struct ExportRevision {
    pub parents: BTreeSet<String>,
    pub content_hash: Option<String>,
    pub size: u64,
    pub worker: String,
}
#[derive(Debug, Clone)]
pub(crate) struct ExportSnapshot {
    pub file: String,
    pub revisions: BTreeSet<String>,
    pub published: bool,
}
#[derive(Debug, Clone)]
pub(crate) struct ExportName {
    pub entries: BTreeMap<String, String>,
    pub parents: BTreeMap<String, BTreeSet<String>>,
    pub published: bool,
}
/// Everything drive history reads from a v7 workspace.
#[derive(Debug, Clone, Default)]
pub(crate) struct Export {
    /// file id -> revision id -> revision (union over every known snapshot).
    pub revisions: BTreeMap<String, BTreeMap<String, ExportRevision>>,
    /// file id -> revision id -> manifest whose bytes are still expected:
    /// retained by a frontier snapshot, else held by a retired snapshot
    /// whose GC has not started in this workspace.
    pub payloads: BTreeMap<String, BTreeMap<String, Manifest>>,
    pub snapshots: BTreeMap<String, ExportSnapshot>,
    pub names: BTreeMap<String, ExportName>,
}

fn export(root: &Path, state: &State, analysis: &model::Analysis) -> Export {
    let mut out = Export::default();
    for (id, snapshot) in &state.snapshots {
        let revisions = out.revisions.entry(snapshot.file_id.clone()).or_default();
        for (rid, r) in &snapshot.revisions {
            revisions
                .entry(rid.clone())
                .or_insert_with(|| ExportRevision {
                    parents: r.parents.clone(),
                    content_hash: r.content_hash.clone(),
                    size: r.size,
                    worker: r.worker.clone(),
                });
        }
        out.snapshots.insert(
            id.clone(),
            ExportSnapshot {
                file: snapshot.file_id.clone(),
                revisions: snapshot.revisions.keys().cloned().collect(),
                published: state.published_snapshots.contains(id),
            },
        );
    }
    for (file, merged) in &analysis.merged {
        let payloads = out.payloads.entry(file.clone()).or_default();
        for (rid, source) in &merged.payloads {
            payloads.insert(rid.clone(), source.manifest.clone());
        }
    }
    for (id, snapshot) in &state.snapshots {
        if !analysis.gc.contains_key(id) || gc_started(root, id) {
            continue;
        }
        let payloads = out.payloads.entry(snapshot.file_id.clone()).or_default();
        for (rid, manifest) in &snapshot.payloads {
            payloads
                .entry(rid.clone())
                .or_insert_with(|| manifest.clone());
        }
    }
    for (id, op) in &state.names {
        out.names.insert(
            id.clone(),
            ExportName {
                entries: op.entries.clone(),
                parents: op.parents.clone(),
                published: state.published_names.contains(id),
            },
        );
    }
    out
}

fn gc_started(root: &Path, snapshot: &str) -> bool {
    root.join("snapshot-gc")
        .join(format!("{snapshot}.json"))
        .exists()
}

/// First time this workspace saw each record (expiry clock of the GC guard).
fn seen_clock(root: &Path, ids: impl Iterator<Item = String>, now: u64) -> BTreeMap<String, u64> {
    let path = root.join("drive-history-seen.json");
    let mut seen: BTreeMap<String, u64> = crate::utils::read_json(&path).unwrap_or_default();
    let before = seen.len();
    for id in ids {
        seen.entry(id).or_insert(now);
    }
    if seen.len() != before {
        if let Err(error) = super::durable_json(&path, &seen) {
            eprintln!("[drive history] cannot save record clock: {error:#}");
        }
    }
    seen
}

impl VirtualDrive {
    /// Current v7 state for drive history (no network access).
    pub(crate) fn snapshot_export(&self) -> Result<Export> {
        let state = self.snapshot_state()?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        Ok(export(&self.root, &state, &analysis))
    }

    /// Retired snapshots whose GC retention defers (see module docs). Fails
    /// closed: on any error every not-yet-started GC is deferred.
    pub(super) fn history_gc_deferred(
        &self,
        state: &State,
        analysis: &model::Analysis,
    ) -> BTreeSet<String> {
        if self.history_retention.is_none() {
            return BTreeSet::new();
        }
        let candidates: BTreeSet<String> = analysis
            .gc
            .keys()
            .filter(|id| !gc_started(&self.root, id))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return candidates;
        }
        match self.deferred_by_retention(state, analysis, &candidates) {
            Ok(deferred) => deferred,
            Err(error) => {
                eprintln!("[drive history] snapshot GC deferred, retention unknown: {error:#}");
                candidates
            }
        }
    }

    fn deferred_by_retention(
        &self,
        state: &State,
        analysis: &model::Analysis,
        candidates: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        use crate::drive_history::{retention, source_v7};
        let now = crate::utils::now_unix();
        let data = export(&self.root, state, analysis);
        let clock = seen_clock(
            &self.root,
            data.snapshots.keys().chain(data.names.keys()).cloned(),
            now,
        );
        // Unreadable marks only mean less is known purged: more is kept.
        let purged = crate::drive_history::marks::Remote::open(
            &self.rclone,
            &self.pool_sync_roots,
            self.policy.native_crypt,
        )
        .and_then(|stores| {
            let stores: Vec<&dyn crate::drive_history::marks::MarkStore> =
                stores.iter().map(|s| s as _).collect();
            crate::drive_history::marks::purged(&stores)
        })
        .unwrap_or_default();
        let history = source_v7::build(&data, &clock, now, purged)?;
        let policy = self.history_retention.unwrap_or_default();
        let keep = retention::protected(&history, &policy, now, &|id| {
            history.revs.get(id).and_then(|r| r.time)
        });
        let mut deferred = BTreeSet::new();
        for key in keep {
            let (file, revision) = source_v7::target(&history, &key)?;
            let retained = analysis
                .merged
                .get(&file)
                .is_some_and(|m| m.payloads.contains_key(&revision));
            if retained {
                continue;
            }
            // Keep one not-yet-collected holder of these bytes.
            if let Some(holder) = candidates.iter().find(|id| {
                let s: &Snapshot = &state.snapshots[*id];
                s.file_id == file && s.payloads.contains_key(&revision)
            }) {
                deferred.insert(holder.clone());
            }
        }
        if !deferred.is_empty() {
            eprintln!(
                "[drive history] retention keeps {} retired snapshot(s) until their versions/trash expire",
                deferred.len()
            );
        }
        Ok(deferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_clock_keeps_first_sighting() {
        let root = tempfile::tempdir().unwrap();
        let first = seen_clock(root.path(), ["a".to_string()].into_iter(), 10);
        assert_eq!(first["a"], 10);
        let second = seen_clock(root.path(), ["a".to_string(), "b".into()].into_iter(), 20);
        assert_eq!((second["a"], second["b"]), (10, 20));
    }
}
