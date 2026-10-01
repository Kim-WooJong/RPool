//! Ctrl-C / SIGTERM handling for the speed test CLI.
//!
//! On macOS and Linux the first SIGINT or SIGTERM only sets a flag (and
//! restores the default action, so a second one ends the process at once).
//! The test then stops its transfers and still deletes its test files. The
//! terminal (Ctrl-C) and the GUI's Stop (SIGTERM to the process group) also
//! stop the running rclone children, so the stop is prompt; cleanup starts
//! fresh rclone processes.
//!
//! Elsewhere (Windows) no handler is installed: Ctrl-C or the GUI's Stop end
//! the process immediately, and a test folder under `.rpool-speedtest/` may
//! remain on the remote being tested (safe to delete by hand).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static SIGNALLED: AtomicBool = AtomicBool::new(false);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn install_handlers() -> bool {
    extern "C" fn on_signal(signal: libc::c_int) {
        SIGNALLED.store(true, Ordering::SeqCst);
        // Async-signal-safe: a second signal takes the default action.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
    }
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores an atomic and calls `signal`, both
    // async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler) != libc::SIG_ERR
            && libc::signal(libc::SIGTERM, handler) != libc::SIG_ERR
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_handlers() -> bool {
    false
}

/// Installs the handlers (where supported) and returns a flag that becomes
/// true on the first signal. Call once per process, from the CLI only.
pub(crate) fn install() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if install_handlers() {
        let watched = flag.clone();
        std::thread::spawn(move || loop {
            if SIGNALLED.load(Ordering::SeqCst) {
                watched.store(true, Ordering::Release);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        });
    }
    flag
}
