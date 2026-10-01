//! Physical cleanup of drive data no one needs any more (`rpool drive
//! cleanup`, and a daily pass of every mount's maintenance loop).
//!
//! Trash expiry, purge and version expiry only hide history; this module
//! deletes the archives (manifest replicas and shards) that no current
//! revision, trash entry, kept version (of any drive generation), open
//! drive on this PC or other reference source needs (`select`). Mirrors
//! the migration cleanup (`migration::retire`): preview by default; a
//! published mark with a grace period (`records`); a later run past the
//! grace re-checks every reference and deletes only what is still
//! unreferenced; unreadable sources postpone everything; mass-delete guard
//! (`execute`). `live`: the cloud side.
pub(crate) mod execute;
pub(crate) mod live;
pub(crate) mod records;
pub(crate) mod select;

#[cfg(test)]
mod tests;

use crate::drive_history::model::{CleanupMode, CleanupReport};
use crate::mount::history_bridge::VirtualDrive;
use crate::prelude::*;
use std::sync::atomic::AtomicBool;

/// What a cleanup run may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub confirm: bool,
    pub force: bool,
    pub cancel: bool,
}

/// One run for `pool`; `local`: this PC's open drive (its unpublished
/// revisions and open files are kept too).
pub(crate) fn run(
    rclone: &str,
    pool: &str,
    local: Option<&VirtualDrive>,
    request: &Request,
) -> Result<CleanupReport> {
    run_with(rclone, pool, local, request, None)
}

fn run_with(
    rclone: &str,
    pool: &str,
    local: Option<&VirtualDrive>,
    request: &Request,
    stop: Option<Arc<AtomicBool>>,
) -> Result<CleanupReport> {
    let mut options = execute::Options::new(
        super::retention::load(pool)?,
        super::retention::load_cleanup(pool)?,
    );
    options.confirm = request.confirm;
    options.force = request.force;
    options.cancel = request.cancel;
    let mut io = live::LiveIo::open(rclone, pool, local)?;
    io.stop = stop;
    execute::run(&io, pool, &options)
}

/// The mount's daily pass: marks new candidates and deletes due data
/// (never forced). `None` when automatic cleanup is off for the pool.
pub(crate) fn auto(drive: &VirtualDrive, stop: Arc<AtomicBool>) -> Option<Result<CleanupReport>> {
    match super::retention::load_cleanup(&drive.pool) {
        Ok(settings) if !settings.auto => return None,
        Ok(_) => {}
        Err(error) => return Some(Err(error)),
    }
    let request = Request {
        confirm: true,
        ..Request::default()
    };
    Some(run_with(
        &drive.rclone,
        &drive.pool,
        Some(drive),
        &request,
        Some(stop),
    ))
}

/// One line for the mount log (`None`: nothing worth printing).
pub(crate) fn log_line(report: &CleanupReport) -> Option<String> {
    let bytes = crate::presentation::format_bytes;
    match report.mode {
        CleanupMode::Postponed => Some(format!(
            "Drive cleanup postponed: {}",
            report.postponed.first().map_or("", String::as_str)
        )),
        _ if report.deleted.archives > 0 || report.candidates.archives > 0 => Some(format!(
            "Drive cleanup: deleted {} ({} archives), marked {} for deletion after the grace period{}",
            bytes(report.deleted.bytes),
            report.deleted.archives,
            bytes(report.candidates.bytes),
            report
                .guard
                .as_ref()
                .map(|g| format!("; due data kept by the mass-delete guard ({g})"))
                .unwrap_or_default()
        )),
        _ => report
            .guard
            .as_ref()
            .map(|g| format!("Drive cleanup: due data kept by the mass-delete guard ({g})")),
    }
}
