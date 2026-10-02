//! Where the Monitoring page gets its data. Production reads the files of
//! running mounts through `crate::monitor`; tests, layout fixtures and debug
//! snapshots supply fixed data instead.
use crate::monitor::model::{HistoryPoint, MountEntry, NetStatus};
use std::path::Path;

/// Data access of the Monitoring page, so tests and snapshots can replace the
/// real mount registry. Held as `MonitoringState::source`.
pub(crate) trait MonitorSource: Send + Sync {
    /// Mounts currently running on this PC.
    fn active_mounts(&self) -> Vec<MountEntry>;
    /// The latest live status of a mount, if one can be read.
    fn read_status(&self, entry: &MountEntry) -> Option<NetStatus>;
    /// Per-minute traffic points of the mount `workspace` from `since_unix` on.
    fn load_history(&self, workspace: &Path, since_unix: u64) -> Vec<HistoryPoint>;
}

/// The running mounts of this PC (registry, status and history files).
#[cfg_attr(test, allow(dead_code))] // Tests never read the real registry.
pub(crate) struct LiveSource;

impl MonitorSource for LiveSource {
    fn active_mounts(&self) -> Vec<MountEntry> {
        crate::monitor::active_mounts()
    }
    fn read_status(&self, entry: &MountEntry) -> Option<NetStatus> {
        crate::monitor::read_status(entry)
    }
    fn load_history(&self, workspace: &Path, since_unix: u64) -> Vec<HistoryPoint> {
        crate::monitor::load_history(workspace, since_unix)
    }
}

/// Fixed mounts with their status and history (tests, fixtures, snapshots).
#[cfg(any(test, debug_assertions))]
#[derive(Default)]
pub(crate) struct FixedSource {
    /// Each mount with its status (if any) and history points.
    pub mounts: Vec<(MountEntry, Option<NetStatus>, Vec<HistoryPoint>)>,
}

#[cfg(any(test, debug_assertions))]
impl MonitorSource for FixedSource {
    fn active_mounts(&self) -> Vec<MountEntry> {
        self.mounts
            .iter()
            .map(|(entry, _, _)| entry.clone())
            .collect()
    }
    fn read_status(&self, entry: &MountEntry) -> Option<NetStatus> {
        self.mounts
            .iter()
            .find(|(e, _, _)| e.id == entry.id)
            .and_then(|(_, status, _)| status.clone())
    }
    fn load_history(&self, workspace: &Path, since_unix: u64) -> Vec<HistoryPoint> {
        self.mounts
            .iter()
            .filter(|(e, _, _)| Path::new(&e.workspace) == workspace)
            .flat_map(|(_, _, history)| history.iter())
            .filter(|point| point.minute_unix >= since_unix)
            .cloned()
            .collect()
    }
}
