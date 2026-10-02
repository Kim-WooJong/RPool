//! Navigation selection: which page, section or drive tab the GUI shows.
//! Stored in `super::GuiState` and switched by the sidebar and tab bars.

/// Top-level pages, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Page {
    /// Overview of pools, accounts and recent activity (start page).
    #[default]
    Dashboard,
    /// The mounted drive and everything used with it (was Storage › Mount).
    Drive,
    /// Inventory, upload and restore of archived files.
    Files,
    /// Providers (accounts), pools and account changes.
    Storage,
    /// Live and past network traffic of the mounted pools.
    Monitoring,
    /// Health checks: archive verify/status, integrity, metadata, diagnostics.
    Maintenance,
    /// Running and past background jobs with their output.
    Jobs,
    /// GUI and rclone settings.
    Settings,
}

/// Sub-sections of [`Page::Files`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FilesSection {
    /// Lists archived files from the local inventory.
    #[default]
    Inventory,
    /// Uploads files as sharded archives.
    Upload,
    /// Restores archives back to local files.
    Restore,
}

/// Sub-sections of [`Page::Storage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StorageSection {
    /// Configured rclone remotes (cloud accounts) and their limits.
    #[default]
    Providers,
    /// Pool definitions (which remotes, shard layout).
    Pools,
    /// Account changes: drain, reprocess, apply to a drive, recover.
    Changes,
}

/// Sub-sections of [`Page::Maintenance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MaintenanceSection {
    /// Verify and status of single archives.
    #[default]
    Archive,
    /// Shard integrity checks across the pool.
    Integrity,
    /// Manifest (metadata) inspection and repair.
    Metadata,
    /// System and environment diagnostics.
    Diagnostics,
}

/// Sub-tabs of the Drive page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DriveTab {
    /// The mounted drive itself: start, stop and status.
    #[default]
    Drive,
    /// Drive mount options.
    Options,
    /// Importing existing data into the drive.
    Import,
    /// Drive maintenance actions.
    Maintenance,
}
