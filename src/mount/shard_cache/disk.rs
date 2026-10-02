//! Free space of the local filesystem that holds the clean cache and spool.
//! `None` means "unknown": callers then apply no disk-pressure policy and rely
//! on their byte budgets and on the operating system's own ENOSPC.
use crate::prelude::*;

/// Free space that disk-pressure relief tries to keep, beyond the bytes that
/// are about to be written. Relief only evicts clean, re-downloadable cache
/// entries; it never refuses a write by itself.
pub(crate) const DISK_FLOOR: u64 = 1024 * 1024 * 1024;

/// Free bytes available to unprivileged users on the filesystem holding
/// `path` (`statvfs`); `None` if unknown.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn available(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is a valid out pointer.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned success and initialised the struct.
    let stat = unsafe { stat.assume_init() };
    // Field widths differ by platform (u32 on macOS, u64 on Linux).
    #[allow(clippy::unnecessary_cast)]
    let (blocks, size) = (stat.f_bavail as u64, stat.f_frsize as u64);
    blocks.checked_mul(size)
}

/// Free bytes available to the caller on the volume holding `path`
/// (`GetDiskFreeSpaceExW`); `None` if unknown.
#[cfg(windows)]
pub(crate) fn available(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated; null pointers are allowed for the
    // totals the call does not need to return.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(crate) fn available(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn free_space_of_a_real_directory_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        assert!(super::available(temp.path()).is_some_and(|n| n > 0));
    }
}
