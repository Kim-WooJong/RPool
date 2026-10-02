//! Speed test choices and the last result per pool for this session (never
//! saved to disk).
use super::plan::{preset_plan, Plan, Preset};
use super::view::ReportView;
use std::collections::BTreeMap;

/// What a run tests: a saved pool, or the remotes ticked on Providers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    Pool(String),
    Remotes,
}

#[derive(Debug)]
pub(crate) struct SpeedTestForm {
    pub(crate) preset: Preset,
    pub(crate) large_mib: u64,
    pub(crate) small_count: usize,
    pub(crate) custom: Plan,
    /// Also find each account's best number of simultaneous uploads.
    pub(crate) tune_uploads: bool,
    /// Remotes ticked for the Providers page test.
    pub(crate) remotes: Vec<String>,
    /// A large run waiting for its confirmation click.
    pub(crate) confirming: Option<Target>,
    /// The target of the running speed test task.
    pub(crate) running: Option<Target>,
    pub(crate) results: BTreeMap<Target, ReportView>,
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
            remotes: Vec::new(),
            confirming: None,
            running: None,
            results: BTreeMap::new(),
            notices: BTreeMap::new(),
        }
    }
}

impl SpeedTestForm {
    pub(crate) fn plan(&self) -> Plan {
        preset_plan(self.preset, self.large_mib, self.small_count, self.custom)
    }
}
