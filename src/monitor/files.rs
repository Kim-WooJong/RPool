//! Locations and atomic writes of the monitoring files.
use super::model::{NetStatus, HISTORY_DIR, STATUS_FILE, STATUS_VERSION};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A status older than this is not live (the mount process died or hangs).
pub(crate) const STATUS_STALE_SECONDS: u64 = 10;

pub(crate) fn metadata_dir(workspace: &Path) -> PathBuf {
    workspace.join(".rpool")
}
pub(crate) fn status_path(workspace: &Path) -> PathBuf {
    metadata_dir(workspace).join(STATUS_FILE)
}
pub(crate) fn history_dir(workspace: &Path) -> PathBuf {
    metadata_dir(workspace).join(HISTORY_DIR)
}

/// Writes `value` as JSON to a temp file next to `path` and renames it over
/// `path`, so readers never see a partial file. No fsync: these files are
/// rewritten every second and are not durable state.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("status file has no parent"))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".net-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    serde_json::to_writer(&mut temp, value).map_err(std::io::Error::other)?;
    temp.flush()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// The live status of `workspace` at `now`: `None` when the file is missing,
/// unparsable, of another version or older than [`STATUS_STALE_SECONDS`].
pub(crate) fn read_status_at(workspace: &Path, now: u64) -> Option<NetStatus> {
    let bytes = std::fs::read(status_path(workspace)).ok()?;
    let status: NetStatus = serde_json::from_slice(&bytes).ok()?;
    (status.version == STATUS_VERSION
        && now.saturating_sub(status.updated_unix) <= STATUS_STALE_SECONDS)
        .then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::model::QueueStatus;

    fn status(pool: &str, updated: u64) -> NetStatus {
        NetStatus {
            version: STATUS_VERSION,
            pool: pool.into(),
            updated_unix: updated,
            uptime_seconds: 5,
            remotes: vec![],
            queue: QueueStatus::default(),
            last_sync_unix: None,
            alerts: vec![],
        }
    }

    #[test]
    fn status_round_trips_atomically_and_goes_stale() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(metadata_dir(root.path())).unwrap();
        assert_eq!(read_status_at(root.path(), 1000), None);
        let written = status("p", 1000);
        write_json_atomic(&status_path(root.path()), &written).unwrap();
        write_json_atomic(&status_path(root.path()), &written).unwrap();
        assert_eq!(read_status_at(root.path(), 1010), Some(written.clone()));
        assert_eq!(read_status_at(root.path(), 1011), None);
        // Only the status file is left behind, no temp files.
        let names: Vec<_> = std::fs::read_dir(metadata_dir(root.path()))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(STATUS_FILE)]);
        std::fs::write(status_path(root.path()), b"{broken").unwrap();
        assert_eq!(read_status_at(root.path(), 1000), None);
        let mut other = written;
        other.version = STATUS_VERSION + 1;
        write_json_atomic(&status_path(root.path()), &other).unwrap();
        assert_eq!(read_status_at(root.path(), 1000), None);
    }
}
