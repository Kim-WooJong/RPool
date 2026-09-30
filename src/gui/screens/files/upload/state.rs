use crate::gui::i18n::{tr, trf};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadTargetMode {
    Pool,
    Manual,
}

impl Default for UploadTargetMode {
    fn default() -> Self {
        Self::Pool
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl UploadItemStatus {
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

#[derive(Debug, Clone)]
pub(crate) struct UploadItem {
    pub(crate) path: PathBuf,
    pub(crate) size: Option<u64>,
    pub(crate) status: UploadItemStatus,
}

impl UploadItem {
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

    pub(crate) fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Default)]
pub(crate) struct UploadForm {
    pub(crate) items: Vec<UploadItem>,
    pub(crate) archive_id: String,
    pub(crate) pool_name: String,
    pub(crate) manual_remote: String,
    pub(crate) target_mode: UploadTargetMode,
    pub(crate) error: Option<String>,
    pub(crate) batch_active: bool,
    pub(crate) active_index: Option<usize>,
    /// Pool mode: this upload's policy instead of the pool's (not saved).
    pub(crate) policy_override: Option<UploadPolicy>,
}

/// One upload's erasure-coding and transfer policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UploadPolicy {
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: crate::models::Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
}
impl UploadPolicy {
    pub(crate) fn of_pool(pool: &crate::models::PoolDefinition) -> Self {
        Self {
            shard_mib: pool.shard_size.mib_ceil(),
            workers: pool.workers,
            retries: pool.retries,
            placement: pool.placement,
            data_shards: pool.data_shards,
            parity_shards: pool.parity_shards,
        }
    }
    pub(crate) fn args(&self) -> Vec<std::ffi::OsString> {
        [
            ("--shard-mib", self.shard_mib.to_string()),
            ("--workers", self.workers.to_string()),
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
    pub(crate) fn for_pools(pool_names: &[String]) -> Self {
        let mut form = Self::default();
        if let [only] = pool_names {
            form.pool_name = only.clone();
        }
        form
    }

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

    pub(crate) fn pending_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == UploadItemStatus::Pending)
            .count()
    }

    pub(crate) fn completed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == UploadItemStatus::Completed)
            .count()
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.items.iter().filter_map(|item| item.size).sum()
    }

    pub(crate) fn clear_queue(&mut self) {
        if self.batch_active {
            return;
        }
        self.items.clear();
        self.active_index = None;
        self.error = None;
    }
}

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
