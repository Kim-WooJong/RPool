//! Network monitoring of mounted pools.
//!
//! Every `rpool mount` process counts the bytes it moves to and from each
//! cloud account (in the rclone layer), writes a live [`model::NetStatus`]
//! once per second into `<workspace>/.rpool/net-status.json`, appends one
//! [`model::HistoryPoint`] per account and minute to
//! `<workspace>/.rpool/net-history/<YYYY-MM-DD>.jsonl` (kept 90 days), and
//! registers itself in `<config>/mounts/<id>.json` ([`model::MountEntry`])
//! while it runs, so the GUI Monitoring page and `rpool mount monitor` find
//! every mounted pool (also ones mounted from the CLI or another GUI).
//! Nothing is monitored for pools that are not mounted.
#![allow(dead_code)] // Contract stub: remove once mount, CLI and GUI use it.
pub(crate) mod model;

use model::{HistoryPoint, MountEntry, NetStatus};
use std::path::Path;

/// Mounts registered by running mount processes (stale entries of dead
/// processes are left out and removed).
pub(crate) fn active_mounts() -> Vec<MountEntry> {
    Vec::new() // Contract stub; implemented by the monitoring work.
}

/// The live status of one mount, `None` when missing, unreadable or stale.
pub(crate) fn read_status(_entry: &MountEntry) -> Option<NetStatus> {
    None // Contract stub.
}

/// History points of `workspace` with `minute_unix >= since_unix`, oldest
/// first.
pub(crate) fn load_history(_workspace: &Path, _since_unix: u64) -> Vec<HistoryPoint> {
    Vec::new() // Contract stub.
}
