use super::capacity::CapacityStatus;
use super::namespace::{durable_json, random_id, valid_path, Intent, Namespace};
use super::shard_cache::ShardCache;
use super::shared_model::{Content, Event};
use super::shared_transport::SharedTransport;
use crate::prelude::*;

mod capacity;
mod move_image;
mod open;
mod read;
mod recovery;
mod rename;
mod run;
mod state_lock;
mod sync;
mod upload_queue;
mod upload_retry;
pub(crate) mod upload_round;
#[cfg(test)]
mod upload_tests;
mod upload_wake;
mod visible;
#[cfg(test)]
mod visible_tests;
mod write;

#[cfg(test)]
pub(crate) use move_image::hooks as move_hooks;
pub(crate) use move_image::StagedMoves;
pub(crate) use open::FORMAT_VERSION;
use recovery::checked_directory;
pub(crate) use recovery::recover_spool;
pub(crate) use run::run;
pub(crate) use state_lock::StateLock;
pub(crate) use upload_retry::RetryBook;
pub(crate) use upload_wake::UploadControl;
#[allow(unused_imports, reason = "API for the frontends that adopt the core")]
pub(crate) use visible::VisibleView;

fn validate_workspace_epoch(epoch: &str) -> Result<()> {
    if epoch.len() != 64
        || !epoch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("invalid workspace metadata epoch");
    }
    Ok(())
}
/// The pool-sync metadata roots a workspace of `pool` on `epoch` uses. Also
/// read by pool change migration.
pub(crate) fn drive_metadata_roots(
    pool: &str,
    remotes: &[String],
    epoch: Option<&str>,
) -> Result<Vec<String>> {
    let mut roots = super::pool_sync::roots(pool, remotes)?;
    if let Some(epoch) = epoch {
        validate_workspace_epoch(epoch)?;
        for root in &mut roots {
            *root = crate::utils::remote_join(root, &format!("epochs/{epoch}"));
        }
    }
    Ok(roots)
}
#[derive(Clone, Debug)]
pub(crate) enum Revision {
    Cloud {
        id: String,
        content: Content,
    },
    Local {
        id: String,
        path: PathBuf,
        size: u64,
        _lease: Arc<()>,
    },
}
impl Revision {
    pub fn id(&self) -> &str {
        match self {
            Self::Cloud { id, .. } | Self::Local { id, .. } => id,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::Cloud { content, .. } => content.size,
            Self::Local { size, .. } => *size,
        }
    }
}
pub(crate) struct VirtualDrive {
    pub root: PathBuf,
    /// The namespace; every mutable access invalidates the visible cache.
    pub state: StateLock,
    pub policy: PoolDefinition,
    pub pool: String,
    pub rclone: String,
    pub cache: ShardCache,
    pub capacity: Mutex<Option<CapacityStatus>>,
    pub sync_gate: Mutex<()>,
    pub pins: Mutex<BTreeMap<String, Revision>>,
    pub local_leases: Mutex<BTreeMap<String, std::sync::Weak<()>>>,
    pub spool_limit: u64,
    /// Serializes spool growth; holds the maintained spool byte count.
    pub spool_writes: Mutex<super::spool::SpoolMeter>,
    /// Pool-sync (v6) metadata roots inside the pool. Every opened workspace
    /// has them; only the test fixture leaves them empty, which keeps the
    /// namespace local (no pull/publish) so tests need no cloud.
    pub pool_sync_roots: Vec<String>,
    /// Revision first served per path; a pool-sync read never mixes revisions.
    pub peer_read_pins: Mutex<BTreeMap<String, String>>,
    /// A pool layout change this mount keeps for later (pending old-layout work).
    pub layout_deferral: Option<super::layout_refresh::Deferral>,
    /// Wakes the mount's uploader when work is queued (`upload_wake`).
    pub upload: UploadControl,
    _lock: File,
}

/// Test-only drive: a local namespace without pool-sync roots, so `pull`
/// and `sync` never touch a cloud (uploads still go through `sync`).
#[cfg(test)]
pub(crate) fn fixture(root: &Path) -> VirtualDrive {
    for path in ["spool", "clean-cache", ".rpool"] {
        fs::create_dir_all(root.join(path)).unwrap();
    }
    let mut state = Namespace::create("tester").unwrap();
    state.save(root).unwrap();
    fixture_with(root, state)
}
/// Reopen a fixture workspace from its saved namespace, as after a process exit.
#[cfg(test)]
pub(crate) fn fixture_reopen(root: &Path) -> VirtualDrive {
    fixture_with(root, Namespace::load(root, "tester").unwrap())
}
#[cfg(test)]
fn fixture_with(root: &Path, state: Namespace) -> VirtualDrive {
    VirtualDrive {
        root: root.into(),
        state: StateLock::new(state),
        policy: PoolDefinition::default(),
        pool: "test".into(),
        rclone: "nonexistent-rclone".into(),
        cache: ShardCache::new(root.join("clean-cache"), 1024).unwrap(),
        capacity: Mutex::new(None),
        sync_gate: Mutex::new(()),
        pins: Mutex::new(BTreeMap::new()),
        local_leases: Mutex::new(BTreeMap::new()),
        spool_limit: 64 * 1073741824,
        spool_writes: Mutex::new(Default::default()),
        pool_sync_roots: vec![],
        peer_read_pins: Mutex::new(BTreeMap::new()),
        layout_deferral: None,
        upload: UploadControl::default(),
        _lock: File::create(root.join("virtual.lock")).unwrap(),
    }
}

#[cfg(test)]
mod policy_refresh_tests {
    use super::*;

    fn policy() -> PoolDefinition {
        PoolDefinition {
            max_object_bytes: None,
            remotes: vec!["one:explicit".into()],
            ..PoolDefinition::default()
        }
    }

    #[test]
    fn metadata_epoch_is_validated_and_does_not_change_legacy_roots() {
        let remotes = policy().remotes;
        let legacy = super::super::pool_sync::roots("pool", &remotes).unwrap();
        assert_eq!(
            drive_metadata_roots("pool", &remotes, None).unwrap(),
            legacy
        );
        let epoch = "a".repeat(64);
        let changed = drive_metadata_roots("pool", &remotes, Some(&epoch)).unwrap();
        assert_eq!(changed[0], format!("{}/epochs/{epoch}", legacy[0]));
        for invalid in ["", "../escape", &"A".repeat(64), &"a".repeat(63)] {
            assert!(drive_metadata_roots("pool", &remotes, Some(invalid)).is_err());
        }
    }
}
