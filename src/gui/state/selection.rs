#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Dashboard,
    Files,
    Storage,
    Jobs,
    Maintenance,
    Settings,
}

impl Default for Page {
    fn default() -> Self {
        Self::Dashboard
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesSection {
    Inventory,
    Upload,
    Restore,
    Verify,
    Status,
}

impl Default for FilesSection {
    fn default() -> Self {
        Self::Inventory
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageSection {
    Providers,
    Pools,
    Reprocess,
    Mount,
}

impl Default for StorageSection {
    fn default() -> Self {
        Self::Providers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenanceSection {
    Integrity,
    Metadata,
    Diagnostics,
}

impl Default for MaintenanceSection {
    fn default() -> Self {
        Self::Integrity
    }
}
