//! Copy-on-write clone of a sealed local image as the baseline of a new spool
//! image: APFS `clonefile` on macOS, `FICLONE` reflink on Linux. A clone
//! shares extents until either side is written, so the mutable new image
//! never changes the immutable sealed one (unlike a hard link). Other
//! filesystems and platforms fall back to a byte copy.
use crate::mount::virtual_drive::VirtualDrive;
use crate::prelude::*;

/// Clone `source`, an immutable sealed image of `len` bytes, to `target` (the
/// not yet existing spool image of a new generation), charging `len` to the
/// spool budget as a copy would. `None` when this filesystem cannot clone or
/// the budget refuses; the caller then copies, which reports a refusal.
pub(super) fn clone_image(
    drive: &VirtualDrive,
    source: &Path,
    target: &Path,
    len: u64,
) -> Result<Option<File>> {
    #[cfg(test)]
    if no_clone::get() {
        return Ok(None);
    }
    let mut meter = drive
        .spool_writes
        .lock()
        .map_err(|_| anyhow!("spool meter lock poisoned"))?;
    if !meter.admit(len, drive.spool_limit, &|| drive.spool_bytes())? {
        return Ok(None);
    }
    let cloned = platform_clone(source, target)
        .ok()
        .filter(|file| file.metadata().is_ok_and(|m| m.len() == len));
    if cloned.is_none() {
        meter.invalidate();
        // Whatever a failed clone left at the fresh spool path is ours.
        let _ = fs::remove_file(target);
    }
    Ok(cloned)
}

#[cfg(target_os = "macos")]
fn platform_clone(source: &Path, target: &Path) -> std::io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    /// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`.
    const NOFOLLOW: u32 = 0x0001;
    let source = CString::new(source.as_os_str().as_bytes())?;
    let path = CString::new(target.as_os_str().as_bytes())?;
    // SAFETY: both arguments are valid NUL-terminated paths for the call.
    if unsafe { libc::clonefile(source.as_ptr(), path.as_ptr(), NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    OpenOptions::new().read(true).write(true).open(target)
}

#[cfg(target_os = "linux")]
fn platform_clone(source: &Path, target: &Path) -> std::io::Result<File> {
    use std::os::fd::AsRawFd;
    let input = File::open(source)?;
    let output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(target)?;
    // SAFETY: both descriptors are open for the duration of the call.
    if unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) } != 0 {
        let error = std::io::Error::last_os_error();
        drop(output);
        let _ = fs::remove_file(target);
        return Err(error);
    }
    Ok(output)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_clone(_: &Path, _: &Path) -> std::io::Result<File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Test switch forcing the byte-copy fallback on the current thread.
#[cfg(test)]
pub(super) mod no_clone {
    use std::cell::Cell;
    thread_local! {
        static OFF: Cell<bool> = const { Cell::new(false) };
    }
    pub(crate) fn set(off: bool) {
        OFF.with(|o| o.set(off));
    }
    pub(super) fn get() -> bool {
        OFF.with(Cell::get)
    }
}
