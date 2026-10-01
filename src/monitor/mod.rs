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
mod alerts;
pub(crate) mod command;
pub(crate) mod files;
pub(crate) mod format;
pub(crate) mod history;
pub(crate) mod model;
pub(crate) mod registry;
pub(crate) mod runtime;
pub(crate) mod sampler;
mod upload_limit;

use model::{HistoryPoint, MountEntry, NetStatus};
use std::path::Path;

/// Mounts registered by running mount processes (stale entries of dead
/// processes are left out and removed), sorted by pool.
pub(crate) fn active_mounts() -> Vec<MountEntry> {
    match registry::registry_dir() {
        Ok(dir) => registry::list(&dir, registry::pid_alive),
        Err(_) => Vec::new(),
    }
}

/// The live status of one mount, `None` when missing, unreadable or stale
/// (not rewritten for [`files::STATUS_STALE_SECONDS`]).
pub(crate) fn read_status(entry: &MountEntry) -> Option<NetStatus> {
    let now = crate::storage::rclone::traffic::now_unix();
    files::read_status_at(Path::new(&entry.workspace), now).filter(|s| s.pool == entry.pool)
}

/// History points of `workspace` with `minute_unix >= since_unix`, oldest
/// first.
pub(crate) fn load_history(workspace: &Path, since_unix: u64) -> Vec<HistoryPoint> {
    history::load(&files::history_dir(workspace), since_unix)
}
