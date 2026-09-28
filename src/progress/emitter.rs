use super::protocol::{ProgressEvent, PROGRESS_ENV, PROGRESS_PREFIX};
use std::sync::Mutex;

static EMIT_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn items(completed_items: usize, total_items: usize) {
    emit(&ProgressEvent::Items {
        completed_items,
        total_items,
    });
}

pub(crate) fn finish() {
    emit(&ProgressEvent::Finish);
}

fn emit(event: &ProgressEvent) {
    if std::env::var_os(PROGRESS_ENV).is_none() {
        return;
    }
    let Ok(payload) = serde_json::to_string(event) else {
        return;
    };
    let _guard = EMIT_LOCK.lock().ok();
    eprintln!("{PROGRESS_PREFIX}{payload}");
}
