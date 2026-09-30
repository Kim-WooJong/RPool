/// Top-level pages, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Page {
    #[default]
    Dashboard,
    /// The mounted drive and everything used with it (was Storage › Mount).
    Drive,
    Files,
    Storage,
    /// Health checks: archive verify/status, integrity, metadata, diagnostics.
    Maintenance,
    Jobs,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FilesSection {
    #[default]
    Inventory,
    Upload,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StorageSection {
    #[default]
    Providers,
    Pools,
    /// Account changes: drain, reprocess, apply to a drive, recover.
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MaintenanceSection {
    /// Verify and status of single archives.
    #[default]
    Archive,
    Integrity,
    Metadata,
    Diagnostics,
}

/// Sub-tabs of the Drive page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DriveTab {
    #[default]
    Drive,
    Options,
    History,
    Import,
    Maintenance,
}
