//! The GUI pages and their forms. `gui::app` routes each `Page` to one of these
//! modules' `show` functions.

/// Dashboard: health, pools, providers and recent jobs at a glance.
#[path = "dashboard/mod.rs"]
pub(crate) mod dashboard;
pub(crate) mod diagnostics_export;
/// Files: Library, Upload, Restore (and the archive verify/status forms).
pub(crate) mod files;
/// Activity page: current operation and task history.
pub(crate) mod jobs;
/// Maintenance: archive check, integrity, metadata and diagnostics.
#[path = "maintenance/mod.rs"]
pub(crate) mod maintenance;
#[path = "monitoring/mod.rs"]
pub(crate) mod monitoring;
pub(crate) mod network_settings;
pub(crate) mod portable_config;
/// Settings page and its tabs.
pub(crate) mod settings;
/// Storage: providers, pools and account changes.
pub(crate) mod storage;
