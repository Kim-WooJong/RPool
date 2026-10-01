//! Pool layout refresh for an existing virtual workspace.
//!
//! The workspace stores the pool policy it last mounted with (`virtual.json`).
//! Resuming a started upload is not self-contained: the upload resume path, the
//! incremental recipe (`policy_hash`) and the publication of an already
//! completed manifest all re-derive shard size, coding and placement from the
//! policy the drive was opened with. So while such work is pending, a mount
//! keeps the previous layout (shard size, placement, K and M) and applies the
//! new layout on the first mount where nothing layout-bound is pending.
//! Execution knobs (workers, retries, object limit, native crypt) apply at once,
//! as before. Membership changes still require "Apply pool changes".
use crate::prelude::*;

/// File the mount writes next to its `--status-file` while a change is deferred.
pub(crate) const STATUS_FILE: &str = "layout-status.json";

/// The layout part of a pool policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Layout {
    pub(crate) shard_size: u64,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
}

impl Layout {
    pub(crate) fn of(policy: &PoolDefinition) -> Self {
        Self {
            shard_size: policy.shard_size.bytes(),
            placement: policy.placement,
            data_shards: policy.data_shards,
            parity_shards: policy.parity_shards,
        }
    }
}

/// A layout change the mount did not apply yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Deferral {
    /// Pending uploads/plans recorded with the previous layout.
    pub(crate) pending: usize,
    /// The layout this mount uses.
    pub(crate) active: Layout,
    /// The pool's saved layout, applied after the pending work finishes.
    pub(crate) requested: Layout,
}

impl Deferral {
    /// The line the CLI prints; the GUI shows the same notice translated.
    pub(crate) fn message(&self) -> String {
        format!(
            "Pool layout change is deferred: {} pending upload(s) use the previous layout; the new layout applies on the next mount after they finish",
            self.pending
        )
    }
}

/// Rejects an invalid pool and remote membership changes.
pub(crate) fn validate_membership(saved: &PoolDefinition, current: &PoolDefinition) -> Result<()> {
    crate::pool::validate_pool(current)?;
    let mut saved_remotes = crate::remote_root::apply_remote_roots(saved.remotes.clone())?;
    let mut current_remotes = crate::remote_root::apply_remote_roots(current.remotes.clone())?;
    saved_remotes.sort();
    current_remotes.sort();
    if saved_remotes != current_remotes {
        bail!("pool membership changed; apply pool changes before mounting this workspace");
    }
    Ok(())
}

/// Counts started work whose resume depends on the workspace's saved layout:
/// spool directories of uncommitted intents that hold an upload plan, a
/// completed but unpublished manifest, an incremental recipe or a v7 snapshot
/// plan, plus unfinished captured uploads of snapshot compaction. Spool left by
/// committed intents is ignored: it is never uploaded again.
pub(crate) fn pending_layout_work<'a>(
    root: &Path,
    pending_intents: impl IntoIterator<Item = &'a str>,
) -> Result<usize> {
    fn scan(path: &Path, matches: &dyn Fn(&Path, &str) -> bool) -> Result<usize> {
        if !path.exists() {
            return Ok(0);
        }
        let mut found = 0;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                bail!("upload state contains a symlink; preserve workspace");
            }
            if kind.is_dir() {
                found += scan(&entry.path(), matches)?;
            } else if matches(path, &entry.file_name().to_string_lossy()) {
                found += 1;
            }
        }
        Ok(found)
    }
    let intent_marker = |dir: &Path, name: &str| {
        name.ends_with(".rpool.upload.json")
            || name.ends_with(".rpool.json")
            || name == "snapshot-plan.json"
            || name == "recipe.json"
                && dir
                    .file_name()
                    .is_some_and(|d| d.to_string_lossy().starts_with("peer-incremental-"))
    };
    let mut pending = 0;
    for id in pending_intents.into_iter().collect::<BTreeSet<_>>() {
        // Intent identities are 64 hex digits; never follow anything else.
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if scan(&root.join("spool").join(id), &intent_marker)? > 0 {
            pending += 1;
        }
    }
    // Compaction plans themselves hold only manifests; their captured uploads
    // carry upload plans until `put` finishes and removes them.
    pending += scan(&root.join("snapshot-compaction"), &|_, name| {
        name.ends_with(".rpool.upload.json")
    })?;
    Ok(pending)
}

/// The policy this mount uses. A layout change applies when nothing
/// layout-bound is pending; otherwise the saved layout is kept for this mount.
pub(crate) fn resolve(
    saved: &PoolDefinition,
    current: &PoolDefinition,
    pending: usize,
) -> Result<(PoolDefinition, Option<Deferral>)> {
    let (active, requested) = (Layout::of(saved), Layout::of(current));
    if active == requested || pending == 0 {
        return Ok((current.clone(), None));
    }
    let mut effective = current.clone();
    effective.shard_size = saved.shard_size;
    effective.placement = saved.placement;
    effective.data_shards = saved.data_shards;
    effective.parity_shards = saved.parity_shards;
    crate::pool::validate_pool(&effective).context(
        "the new pool settings cannot be combined with the previous layout that pending uploads still use; restore the previous object limit until they finish. Pending data retained",
    )?;
    Ok((
        effective,
        Some(Deferral {
            pending,
            active,
            requested,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PoolDefinition {
        PoolDefinition {
            max_object_bytes: None,
            remotes: vec!["one:explicit".into()],
            ..PoolDefinition::default()
        }
    }
    fn larger_shards(policy: &PoolDefinition) -> PoolDefinition {
        let mut changed = policy.clone();
        changed.shard_size = crate::models::shard_size::ShardSize::from_mib(
            policy.shard_size.exact_mib().unwrap() + 1,
        )
        .unwrap();
        changed
    }
    const ID: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    fn spool_file(root: &Path, relative: &str) {
        let path = root.join("spool").join(ID).join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"saved").unwrap();
    }

    #[test]
    fn unchanged_layout_applies_execution_knobs_even_with_pending_plans() {
        let root = tempfile::tempdir().unwrap();
        spool_file(root.path(), "upload/eligible-k/f.rpool.upload.json");
        let saved = policy();
        let mut current = saved.clone();
        current.workers += 1;
        current.retries += 1;
        let pending = pending_layout_work(root.path(), [ID]).unwrap();
        assert_eq!(pending, 1);
        let (effective, deferral) = resolve(&saved, &current, pending).unwrap();
        assert_eq!(effective.workers, current.workers);
        assert!(deferral.is_none());
    }

    #[test]
    fn changed_layout_without_pending_work_applies_immediately() {
        let root = tempfile::tempdir().unwrap();
        // Spool content not yet planned is uploaded with the new layout, and
        // leftovers of committed intents are never uploaded again.
        spool_file(root.path(), "content");
        let other = "0000000000000000000000000000000000000000000000000000000000000002";
        let stale = root.path().join("spool").join(other);
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("f.rpool.upload.json"), b"stale").unwrap();
        let saved = policy();
        let current = larger_shards(&saved);
        let pending = pending_layout_work(root.path(), [ID]).unwrap();
        assert_eq!(pending, 0);
        let (effective, deferral) = resolve(&saved, &current, pending).unwrap();
        assert_eq!(Layout::of(&effective), Layout::of(&current));
        assert!(deferral.is_none());
    }

    #[test]
    fn changed_layout_with_pending_plans_keeps_previous_layout() {
        for marker in [
            "upload/eligible-k/f.rpool.upload.json",
            "upload/eligible-k/f.rpool.json",
            "upload/peer-incremental-virtual-x/recipe.json",
            "snapshot-plan.json",
        ] {
            let root = tempfile::tempdir().unwrap();
            spool_file(root.path(), marker);
            let saved = policy();
            let mut current = larger_shards(&saved);
            current.parity_shards += 1;
            current.workers += 2;
            let pending = pending_layout_work(root.path(), [ID, ID]).unwrap();
            assert_eq!(pending, 1, "{marker}");
            let (effective, deferral) = resolve(&saved, &current, pending).unwrap();
            assert_eq!(Layout::of(&effective), Layout::of(&saved), "{marker}");
            assert_eq!(effective.workers, current.workers);
            let deferral = deferral.unwrap();
            assert_eq!(deferral.requested, Layout::of(&current));
            assert!(deferral.message().contains("1 pending upload(s)"));
        }
        // An unrelated file named recipe.json is not an incremental recipe.
        let root = tempfile::tempdir().unwrap();
        spool_file(root.path(), "upload/recipe.json");
        assert_eq!(pending_layout_work(root.path(), [ID]).unwrap(), 0);
    }

    #[test]
    fn snapshot_compaction_defers_only_while_a_captured_upload_is_unfinished() {
        let root = tempfile::tempdir().unwrap();
        let compact = root.path().join("snapshot-compaction");
        fs::create_dir_all(compact.join("captured/r/eligible-k")).unwrap();
        fs::write(compact.join("plan.json"), b"plan").unwrap();
        fs::write(compact.join("captured/r/content"), b"bytes").unwrap();
        let none: [&str; 0] = [];
        assert_eq!(pending_layout_work(root.path(), none).unwrap(), 0);
        let plan = compact.join("captured/r/eligible-k/content.rpool.upload.json");
        fs::write(&plan, b"plan").unwrap();
        assert_eq!(pending_layout_work(root.path(), none).unwrap(), 1);
        let saved = policy();
        let current = larger_shards(&saved);
        assert!(resolve(&saved, &current, 1).unwrap().1.is_some());
        fs::remove_file(plan).unwrap();
        assert_eq!(pending_layout_work(root.path(), none).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_upload_state_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        spool_file(root.path(), "content");
        std::os::unix::fs::symlink("/", root.path().join("spool").join(ID).join("link")).unwrap();
        assert!(pending_layout_work(root.path(), [ID]).is_err());
    }

    #[test]
    fn membership_change_and_invalid_policy_are_rejected() {
        let saved = policy();
        let mut current = larger_shards(&saved);
        validate_membership(&saved, &current).unwrap();
        current.remotes.push("two:explicit".into());
        assert!(validate_membership(&saved, &current).is_err());
        current = saved.clone();
        current.workers = 0;
        assert!(validate_membership(&saved, &current).is_err());
    }
}
