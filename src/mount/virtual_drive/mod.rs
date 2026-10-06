//! The mounted pool-sync drive (`VirtualDrive`): workspace state, the
//! namespace, clean shard cache, spool and the wakers of its background
//! uploader and publisher. Submodules implement opening, reads, writes,
//! renames, upload queue/rounds and sync; frontends (`dav`, `fs_core`) call
//! into it. Entry point for `rpool mount`: [`run`].

use super::capacity::CapacityStatus;
use super::namespace::{durable_json, random_id, valid_path, Intent, Namespace};
use super::shard_cache::ShardCache;
use super::shared_model::{Content, Event};
use super::shared_transport::SharedTransport;
use crate::prelude::*;

mod add_accounts;
mod capacity;
/// How long a capacity snapshot serves the free-space report (tests).
#[cfg(test)]
pub(crate) use capacity::SNAPSHOT_MAX_AGE;
mod move_image;
mod open;
/// Small-file packs: many small writes uploaded as one archive.
mod pack;
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

/// Checks a metadata epoch is 64 lowercase hex digits.
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
/// A file revision a read is served from (pinned per path while open).
#[derive(Clone, Debug)]
pub(crate) enum Revision {
    /// A committed revision stored in the pool.
    Cloud {
        /// Namespace event id.
        id: String,
        /// Content (manifest) to read through the shard cache.
        content: Content,
    },
    /// A local spool image not yet committed (or committed but kept until clean).
    Local {
        /// Intent id.
        id: String,
        /// Spool image file.
        path: PathBuf,
        /// Image size in bytes.
        size: u64,
        /// Keeps the spool image from committed-spool cleanup while held
        /// (see `local_lease`).
        _lease: Arc<()>,
    },
}
impl Revision {
    /// Event id (cloud) or intent id (local).
    pub fn id(&self) -> &str {
        match self {
            Self::Cloud { id, .. } | Self::Local { id, .. } => id,
        }
    }
    /// Revision size in bytes.
    pub fn size(&self) -> u64 {
        match self {
            Self::Cloud { content, .. } => content.size,
            Self::Local { size, .. } => *size,
        }
    }
}
/// One open drive workspace, shared (`Arc`) by the frontend and the
/// background uploader, publisher and maintenance threads.
pub(crate) struct VirtualDrive {
    /// Workspace directory (namespace, spool, cache, `virtual.json`).
    pub root: PathBuf,
    /// The namespace; every mutable access invalidates the visible cache.
    pub state: StateLock,
    /// Pool definition the drive uses (placement, workers, native crypt...).
    pub policy: PoolDefinition,
    /// Pool name.
    pub pool: String,
    /// rclone binary.
    pub rclone: String,
    /// Clean, verified shard cache for reads.
    pub cache: ShardCache,
    /// Last capacity snapshot (`None` until measured or after expiry).
    pub capacity: Mutex<Option<CapacityStatus>>,
    /// Serializes pull/sync rounds and metadata compaction.
    pub sync_gate: Mutex<()>,
    /// Revision currently served per open path, kept until released.
    pub pins: Mutex<BTreeMap<String, Revision>>,
    /// Weak leases of local spool images in use (intent id -> lease); a live
    /// lease blocks cleanup of that image.
    pub local_leases: Mutex<BTreeMap<String, std::sync::Weak<()>>>,
    /// Spool byte budget (`--spool-gib`).
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
    /// Wakes the mount's metadata publisher after a commit (`publisher`).
    pub publish: UploadControl,
    /// Serializes metadata publication and committed-spool cleanup.
    pub publish_gate: Mutex<()>,
    /// Set while the mount's publisher thread runs: upload passes then only
    /// wake it instead of publishing inline (new uploads never wait for it).
    pub publisher_running: std::sync::atomic::AtomicBool,
    /// Exclusive workspace lock file (`virtual.lock`), held while open.
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
        publish: UploadControl::default(),
        publish_gate: Mutex::new(()),
        publisher_running: std::sync::atomic::AtomicBool::new(false),
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
