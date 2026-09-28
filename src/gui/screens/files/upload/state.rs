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
            Self::Pending => "Pending",
            Self::Running => "Uploading",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
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
        let size = path.metadata().ok().filter(|meta| meta.is_file()).map(|meta| meta.len());
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
            self.error = Some(format!(
                "{rejected} dropped item(s) were ignored because only local files can be uploaded."
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
