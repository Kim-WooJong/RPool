//! Emits machine-readable progress events on stderr when the parent process
//! (the GUI task runner) set [`PROGRESS_ENV`]. Used by long CLI operations
//! such as reprocess, scan/repair and speed tests.
use super::protocol::{ProgressEvent, PROGRESS_ENV, PROGRESS_PREFIX};
use std::io::Write;
use std::sync::Mutex;

/// Serializes writes so concurrent workers never interleave event lines.
static EMIT_LOCK: Mutex<()> = Mutex::new(());

/// Reports item-level progress (`completed_items` of `total_items`), e.g. files
/// processed by reprocess or maintenance scan/repair.
pub(crate) fn items(completed_items: usize, total_items: usize) {
    emit(&ProgressEvent::Items {
        completed_items,
        total_items,
    });
}

/// Announces the total byte count of the operation; sent once at the start
/// (speed test run).
pub(crate) fn start(total_bytes: u64) {
    emit(&ProgressEvent::Start { total_bytes });
}

/// Reports byte progress: `completed_bytes` of the planned total and
/// `transferred_bytes` actually moved since the previous event.
pub(crate) fn advance(completed_bytes: u64, transferred_bytes: u64) {
    emit(&ProgressEvent::Advance {
        completed_bytes,
        transferred_bytes,
    });
}

/// Whether a GUI (or other parent) asked for progress events.
pub(crate) fn enabled() -> bool {
    std::env::var_os(PROGRESS_ENV).is_some()
}

/// Signals that the operation finished, so the GUI can close its progress bar.
pub(crate) fn finish() {
    emit(&ProgressEvent::Finish);
}

/// Writes one `PROGRESS_PREFIX` + JSON line to stderr; a no-op when progress is
/// disabled. Serialization or write failures are ignored.
fn emit(event: &ProgressEvent) {
    if !enabled() {
        return;
    }
    let Ok(payload) = serde_json::to_string(event) else {
        return;
    };
    let _guard = EMIT_LOCK.lock().ok();
    // A closed stderr must not panic the command (e.g. mid-cleanup).
    let _ = writeln!(std::io::stderr(), "{PROGRESS_PREFIX}{payload}");
}
