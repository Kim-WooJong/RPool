//! Shutdown requests from outside for the `rpool mount` process: SIGTERM,
//! SIGINT (Ctrl-C) and SIGHUP on macOS/Linux, console close/Ctrl-C/logoff/
//! shutdown events on Windows.
//!
//! Without a handler the process died on the spot, so the graceful stop
//! (flush delayed saves, `core/quit` to rclone, wait for the unmount) never
//! ran and rclone was left behind with its mount. Now the first request only
//! sets a flag that `lifecycle::StopControl` treats like the stop file; a
//! second one takes the default action (immediate exit). Installed by
//! `virtual_drive::run` for mounts only.
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the handler once a shutdown was requested.
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set when the mount finished its graceful stop (Windows close events wait for it).
static FINISHED: AtomicBool = AtomicBool::new(false);

/// Whether a shutdown signal arrived.
pub(crate) fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}

/// Marks the graceful stop done when dropped (see [`install`]).
pub(crate) struct Finished;
impl Drop for Finished {
    fn drop(&mut self) {
        FINISHED.store(true, Ordering::Release);
    }
}

/// Installs the handlers; the returned guard marks the stop finished when
/// the mount run ends. Failure to install only loses the graceful path.
pub(crate) fn install() -> Finished {
    if !install_platform() {
        eprintln!("[warning] shutdown signals are not handled; stop the mount from RPool to unmount cleanly");
    }
    Finished
}

#[cfg(unix)]
/// SIGINT/SIGTERM/SIGHUP set [`REQUESTED`]; the handler then restores the
/// default action, so a second signal ends the process at once.
fn install_platform() -> bool {
    extern "C" fn on_signal(signal: libc::c_int) {
        REQUESTED.store(true, Ordering::Release);
        // SAFETY: `signal` is async-signal-safe.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
    }
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores an atomic and calls `signal`, both
    // async-signal-safe.
    unsafe {
        [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .iter()
            .all(|signal| libc::signal(*signal, handler) != libc::SIG_ERR)
    }
}

#[cfg(windows)]
/// Console control events set [`REQUESTED`]. For close, logoff and shutdown
/// Windows ends the process when the handler returns, so it waits (at most
/// about 4.5 s, below the system's 5 s) for the graceful stop.
fn install_platform() -> bool {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };
    unsafe extern "system" fn on_event(kind: u32) -> BOOL {
        REQUESTED.store(true, Ordering::Release);
        if kind != CTRL_C_EVENT && kind != CTRL_BREAK_EVENT {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(4500);
            while !FINISHED.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        TRUE
    }
    // SAFETY: registers a handler with the documented signature.
    unsafe { SetConsoleCtrlHandler(Some(on_event), TRUE) != 0 }
}

#[cfg(not(any(unix, windows)))]
/// No handlers on other platforms.
fn install_platform() -> bool {
    false
}
