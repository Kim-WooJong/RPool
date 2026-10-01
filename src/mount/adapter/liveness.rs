//! Platform-specific check whether a recorded mount PID is still alive.

use super::*;

#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> Result<bool> {
    let pid = i32::try_from(pid).context("invalid recorded mount PID")?;
    if pid <= 0 {
        bail!("invalid recorded mount PID");
    }
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // Signal zero performs existence/access checking only; it never sends a signal.
    if unsafe { kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        Ok(false)
    } else {
        Err(error.into())
    }
}

#[cfg(windows)]
pub(crate) fn process_alive(pid: u32) -> Result<bool> {
    if pid == 0 {
        bail!("invalid recorded mount PID");
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) }; // SYNCHRONIZE
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == 87 {
            return Ok(false);
        } // nonexistent process: ERROR_INVALID_PARAMETER
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    let result = unsafe { WaitForSingleObject(handle, 0) };
    let error = if result == 0xffff_ffff {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        CloseHandle(handle);
    }
    match result {
        0 => Ok(false),  // process handle signaled: exited
        258 => Ok(true), // WAIT_TIMEOUT
        _ => Err(error
            .unwrap_or_else(|| std::io::Error::other("unexpected process wait result"))
            .into()),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_alive(_: u32) -> Result<bool> {
    bail!("process liveness unavailable on this platform")
}
