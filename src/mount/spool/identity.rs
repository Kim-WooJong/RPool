//! Physical identity of spool files, so the budget scan counts an image that
//! a MOVE hard-linked into a second spool directory once.
use crate::prelude::*;

/// `(volume, file)` identity of a regular file that has more than one link;
/// `None` for a file with a single link, whose bytes count where it is found.
#[cfg(unix)]
pub(super) fn shared_identity(_path: &Path, meta: &fs::Metadata) -> Result<Option<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    #[allow(clippy::unnecessary_cast, reason = "dev_t width differs by platform")]
    Ok((meta.nlink() > 1).then(|| (meta.dev() as u64, meta.ino())))
}

#[cfg(windows)]
pub(super) fn shared_identity(path: &Path, _meta: &fs::Metadata) -> Result<Option<(u64, u64)>> {
    use std::os::windows::io::AsRawHandle;
    /// `BY_HANDLE_FILE_INFORMATION`.
    #[repr(C)]
    struct Information {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut Information,
        ) -> i32;
    }
    let file = File::open(path)?;
    let mut information = std::mem::MaybeUninit::<Information>::uninit();
    // SAFETY: the handle is open for the call and `information` is a valid
    // out pointer of the documented layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the call succeeded and filled the struct.
    let information = unsafe { information.assume_init() };
    let index = (u64::from(information.index_high) << 32) | u64::from(information.index_low);
    Ok((information.links > 1).then_some((u64::from(information.volume), index)))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn shared_identity(_path: &Path, _meta: &fs::Metadata) -> Result<Option<(u64, u64)>> {
    Ok(None)
}
