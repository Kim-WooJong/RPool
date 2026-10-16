//! Cloud `StorageIo` of one provider root: the journal on the provider,
//! fresh observation and classification, deletion through the crypt remote,
//! and removal of emptied folders on backends that keep them.
use super::classify::{classify, Classified};
use super::execute::StorageIo;
use crate::drive_history::cleanup::records::{self, Record};
use crate::drive_history::marks::MarkStore;
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::OperationContext;

/// Backends whose empty folders stay behind after their files are deleted.
const FOLDER_BACKENDS: &[&str] = &["sftp", "smb", "local", "ftp"];

/// The provider `remote` (`name`, `name:` or `name:path`) resolved with its
/// default path; errors unless it is an encrypting crypt remote.
pub(crate) fn resolve(rclone: &str, remote: &str) -> Result<String> {
    let remote = remote.trim();
    let remote = if remote.contains(':') {
        remote.to_owned()
    } else {
        format!("{remote}:")
    };
    let root = crate::remote_root::apply_remote_root(&remote)?;
    RcloneAdmin::inherited(rclone)
        .ensure_encrypted(&root)
        .with_context(|| format!("{root} is not an encrypting crypt remote"))?;
    Ok(root.trim_end_matches('/').to_owned())
}

/// Cloud `StorageIo` for one provider root. Opened by `storage::list` and
/// `storage::clean`.
pub(crate) struct LiveIo {
    /// rclone executable.
    rclone: String,
    /// Provider root (`remote:path`).
    root: String,
    /// Journal folder on the provider.
    journal: super::journal::Store,
    /// Name of this PC.
    worker: String,
    /// Parallel deletions.
    workers: usize,
    /// The backend keeps empty folders (remove them after deleting).
    folders: bool,
    /// Grace period in days.
    grace_days: u32,
    /// Seconds added to the clock (tests run "days later").
    pub(crate) clock_offset: u64,
}

impl LiveIo {
    /// Opens the provider `remote`; the grace period is the longest drive
    /// cleanup grace period of the pools using it (default when none).
    pub(crate) fn open(rclone: &str, remote: &str, workers: usize) -> Result<Self> {
        let root = resolve(rclone, remote)?;
        let store = crate::pool::load_pool_store()?;
        let mut grace_days = None;
        for (name, policy) in &store.pools {
            let roots = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
            if roots.iter().any(|r| super::observe::same_root(r, &root)) {
                let days = store
                    .drive_cleanup
                    .get(name)
                    .copied()
                    .unwrap_or_default()
                    .grace_days;
                grace_days = grace_days.max(Some(days));
            }
        }
        let folders = RcloneAdmin::inherited(rclone)
            .catalog()
            .ok()
            .and_then(|catalog| {
                let base = catalog.placement_target(&root).ok()?;
                catalog.backend_kind(&base).map(str::to_owned)
            })
            .is_some_and(|kind| FOLDER_BACKENDS.contains(&kind.as_str()));
        Ok(Self {
            rclone: rclone.into(),
            journal: super::journal::Store::open(rclone, &root)?,
            root,
            worker: crate::drive_history::dispatch::cloud_worker(),
            workers: workers.max(1),
            folders,
            grace_days: grace_days
                .unwrap_or(crate::drive_history::model::DEFAULT_CLEANUP_GRACE_DAYS),
            clock_offset: 0,
        })
    }

    /// Names at the provider root that do not decrypt (rclone skips them in
    /// listings): the base folder's entries minus the crypt view's. Best
    /// effort; `None` when it cannot be counted.
    pub(crate) fn undecryptable(&self) -> Option<u64> {
        let context = RcloneContext::inherited(&self.rclone);
        let ctx = || {
            OperationContext::with_deadline(
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
        };
        let (name, path) = self.root.split_once(':')?;
        let config = context.config_dump(&ctx()).ok()?;
        let backing = config.get(name)?.get("remote")?.as_str()?.to_owned();
        let base = if path.trim_matches('/').is_empty() {
            backing
        } else {
            context
                .crypt_base_object(&ctx(), &self.root, &backing)
                .ok()?
        };
        let count = |address: &str| -> Option<u64> {
            let bytes = context
                .capture(&ctx(), &["lsf", "--max-depth", "1", "--", address])
                .ok()?;
            Some(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count() as u64,
            )
        };
        Some(count(&base)?.saturating_sub(count(&self.root)?))
    }
}

impl StorageIo for LiveIo {
    fn root(&self) -> String {
        self.root.clone()
    }
    fn records(&self) -> Result<Vec<Record>> {
        records::read(&[&self.journal as &dyn MarkStore])
    }
    fn publish(&self, record: &Record) -> Result<()> {
        records::publish(&[&self.journal as &dyn MarkStore], record)
    }
    fn observe(&self) -> Result<Classified> {
        let world = super::observe::world(&self.rclone, &self.root, self.now())?;
        Ok(classify(&world))
    }
    fn delete(&self, address: &str) -> Result<()> {
        crate::migration::retire::live::delete_object(&self.rclone, false, address)
    }
    fn remove_dir(&self, address: &str) {
        if self.folders {
            let context = RcloneContext::inherited(&self.rclone);
            let _ = context.rmdir_raw(&OperationContext::none(), address);
        }
    }
    fn now(&self) -> u64 {
        crate::utils::now_unix() + self.clock_offset
    }
    fn worker(&self) -> String {
        self.worker.clone()
    }
    fn grace_days(&self) -> u32 {
        self.grace_days
    }
    fn workers(&self) -> usize {
        self.workers
    }
}
