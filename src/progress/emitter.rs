use super::protocol::{ProgressEvent, PROGRESS_ENV, PROGRESS_PREFIX};
use std::io::Write;
use std::sync::Mutex;

static EMIT_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn items(completed_items: usize, total_items: usize) {
    emit(&ProgressEvent::Items {
        completed_items,
        total_items,
    });
}

pub(crate) fn start(total_bytes: u64) {
    emit(&ProgressEvent::Start { total_bytes });
}

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

pub(crate) fn finish() {
    emit(&ProgressEvent::Finish);
}

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
