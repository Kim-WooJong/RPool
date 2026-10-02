//! Speed test choices and the last result per pool for this session (never
//! saved to disk).
use super::plan::{preset_plan, Plan, Preset};
use super::view::ReportView;
use std::collections::BTreeMap;

/// What a run tests: a saved pool, or the remotes ticked on Providers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    /// The saved pool with this name (Pools page card).
    Pool(String),
    /// The remotes ticked on the Providers page card.
    Remotes,
}

/// Speed test choices and results, held in `GuiState::speed_test`; shared by
/// the Pools and Providers page cards, results kept per target.
#[derive(Debug)]
pub(crate) struct SpeedTestForm {
    /// Selected size preset.
    pub(crate) preset: Preset,
    /// Size per account of the "Large file" preset, MiB (one of `LARGE_SIZES_MIB`).
    pub(crate) large_mib: u64,
    /// File count of the "Many small files" preset (one of `SMALL_COUNTS`).
    pub(crate) small_count: usize,
    /// Size and file count of the "Custom" preset.
    pub(crate) custom: Plan,
    /// Also find each account's best number of simultaneous uploads.
    pub(crate) tune_uploads: bool,
    /// Also find each account's best number of simultaneous shard downloads.
    pub(crate) tune_downloads: bool,
    /// Remotes ticked for the Providers page test.
    pub(crate) remotes: Vec<String>,
    /// A large run waiting for its confirmation click.
    pub(crate) confirming: Option<Target>,
    /// The target of the running speed test task.
    pub(crate) running: Option<Target>,
    /// Last report per target, prepared for display.
    pub(crate) results: BTreeMap<Target, ReportView>,
    /// Start error or outcome message per target.
    pub(crate) notices: BTreeMap<Target, String>,
}

impl Default for SpeedTestForm {
    fn default() -> Self {
        Self {
            preset: Preset::Quick,
            large_mib: 1024,
            small_count: 256,
            custom: Plan {
                size_mib: 16,
                files: 4,
            },
            tune_uploads: false,
            tune_downloads: false,
            remotes: Vec::new(),
            confirming: None,
            running: None,
            results: BTreeMap::new(),
            notices: BTreeMap::new(),
        }
    }
}

impl SpeedTestForm {
    /// Plan of the selected preset (not yet validated).
    pub(crate) fn plan(&self) -> Plan {
        preset_plan(self.preset, self.large_mib, self.small_count, self.custom)
    }
}
