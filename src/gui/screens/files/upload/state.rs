//! State of Files › Upload (`UploadForm`): the file queue, target choice,
//! per-upload policy override and batch progress.

use crate::gui::i18n::{tr, trf};
use std::path::{Path, PathBuf};

/// Where uploads go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UploadTargetMode {
    /// A configured storage pool (the normal case).
    #[default]
    Pool,
    /// Manual one-off: remotes and policy from the settings.
    Manual,
}

/// Progress of one queued file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadItemStatus {
    /// Waiting to be uploaded.
    Pending,
    /// Its `rpool put` task is running.
    Running,
    /// Uploaded successfully.
    Completed,
    /// The task failed; the batch paused.
    Failed,
    /// The task was cancelled; the batch stopped.
    Cancelled,
}

impl UploadItemStatus {
    /// Translated status badge text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pending => tr("Pending"),
            Self::Running => tr("Uploading"),
            Self::Completed => tr("Completed"),
            Self::Failed => tr("Failed"),
            Self::Cancelled => tr("Cancelled"),
        }
    }
}

/// One file in the upload queue.
#[derive(Debug, Clone)]
pub(crate) struct UploadItem {
    /// Local path of the file.
    pub(crate) path: PathBuf,
    /// Size in bytes when added (`None` if it could not be read).
    pub(crate) size: Option<u64>,
    /// Upload progress of this file.
    pub(crate) status: UploadItemStatus,
}

impl UploadItem {
    /// A pending item, reading the file size from its metadata.
    fn new(path: PathBuf) -> Self {
        let size = path
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len());
        Self {
            path,
            size,
            status: UploadItemStatus::Pending,
        }
    }

    /// File name for display (falls back to the whole path).
    pub(crate) fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Whole state of Files › Upload, in `GuiState::upload`.
#[derive(Debug, Default)]
pub(crate) struct UploadForm {
    /// Queued files, in upload order.
    pub(crate) items: Vec<UploadItem>,
    /// Archive ID typed for a single-file upload (`""` = generated); cleared when
    /// more than one file is queued.
    pub(crate) archive_id: String,
    /// Target pool in pool mode.
    pub(crate) pool_name: String,
    /// Remote being typed in the manual destination selector.
    pub(crate) manual_remote: String,
    /// Pool or manual target.
    pub(crate) target_mode: UploadTargetMode,
    /// Last start or batch error, shown under the summary.
    pub(crate) error: Option<String>,
    /// Whether a batch is uploading the queue.
    pub(crate) batch_active: bool,
    /// Queue index of the file currently uploading.
    pub(crate) active_index: Option<usize>,
    /// Pool mode: this upload's policy instead of the pool's (not saved).
    pub(crate) policy_override: Option<UploadPolicy>,
}

/// One upload's erasure-coding and transfer policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UploadPolicy {
    /// Shard size in MiB (`--shard-mib`).
    pub(crate) shard_mib: u64,
    /// Retry count passed as `--retries`.
    pub(crate) retries: u32,
    /// Shard placement strategy (`--placement`).
    pub(crate) placement: crate::models::Placement,
    /// Data shards K (`--data-shards`).
    pub(crate) data_shards: usize,
    /// Parity shards M (`--parity-shards`; 0 = no erasure coding).
    pub(crate) parity_shards: usize,
}
impl UploadPolicy {
    /// The pool's own policy, the starting values of an override.
    pub(crate) fn of_pool(pool: &crate::models::PoolDefinition) -> Self {
        Self {
            shard_mib: pool.shard_size.mib_ceil(),
            retries: pool.retries,
            placement: pool.placement,
            data_shards: pool.data_shards,
            parity_shards: pool.parity_shards,
        }
    }
    /// The policy as `rpool put` flags.
    pub(crate) fn args(&self) -> Vec<std::ffi::OsString> {
        [
            ("--shard-mib", self.shard_mib.to_string()),
            ("--placement", self.placement.cli_value().to_string()),
            ("--retries", self.retries.to_string()),
            ("--data-shards", self.data_shards.to_string()),
            ("--parity-shards", self.parity_shards.to_string()),
        ]
        .into_iter()
        .flat_map(|(flag, value)| [flag.into(), value.into()])
        .collect()
    }
}

impl UploadForm {
    /// Empty form, preselecting the pool when exactly one exists.
    /// Called when the GUI state is created.
    pub(crate) fn for_pools(pool_names: &[String]) -> Self {
        let mut form = Self::default();
        if let [only] = pool_names {
            form.pool_name = only.clone();
        }
        form
    }

    /// Adds local files to the queue (non-files are rejected with a message,
    /// duplicates skipped). Returns how many were added.
    pub(crate) fn add_paths<I>(&mut self, paths: I) -> usize
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut added = 0;
        let mut rejected = 0;
        for path in paths {
            if !path.is_file() {
                rejected += 1;
                continue;
            }
            if self.items.iter().any(|item| same_path(&item.path, &path)) {
                continue;
            }
            self.items.push(UploadItem::new(path));
            added += 1;
        }

        if self.items.len() > 1 {
            self.archive_id.clear();
        }

        if rejected > 0 {
            self.error = Some(trf(
                "{n} dropped item(s) were ignored because only local files can be uploaded.",
                &[("n", &rejected)],
            ));
        } else if added > 0 {
            self.error = None;
        }
        added
    }

    /// Number of items still waiting.
    pub(crate) fn pending_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == UploadItemStatus::Pending)
            .count()
    }

    /// Number of items uploaded successfully.
    pub(crate) fn completed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == UploadItemStatus::Completed)
            .count()
    }

    /// Sum of the known item sizes in bytes.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.items.iter().filter_map(|item| item.size).sum()
    }

    /// Empties the queue and clears the error; does nothing while a batch runs.
    pub(crate) fn clear_queue(&mut self) {
        if self.batch_active {
            return;
        }
        self.items.clear();
        self.active_index = None;
        self.error = None;
    }
}

/// Path equality for duplicate detection (ASCII case-insensitive on Windows).
fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }

    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn pool_override_flags_parse_with_pool() {
        use clap::Parser;
        let pool = crate::models::PoolDefinition {
            data_shards: 9,
            parity_shards: 3,
            ..Default::default()
        };
        let mut policy = UploadPolicy::of_pool(&pool);
        policy.parity_shards = 4;
        let argv = ["rpool", "put", "file.bin", "--pool", "family"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .chain(policy.args());
        let parsed = crate::cli::Cli::try_parse_from(argv).unwrap();
        let Some(crate::cli::Commands::Put {
            parity_shards,
            pool,
            ..
        }) = parsed.command
        else {
            panic!("put")
        };
        assert_eq!((parity_shards, pool.as_deref()), (Some(4), Some("family")));
    }
}
