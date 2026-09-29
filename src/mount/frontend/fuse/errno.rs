//! `FsError` → POSIX errno.
use crate::mount::fs_core::FsError;
use fuser::Errno;

pub(super) fn errno(error: FsError) -> Errno {
    match error {
        FsError::NotFound => Errno::ENOENT,
        FsError::Exists => Errno::EEXIST,
        FsError::IsDir => Errno::EISDIR,
        FsError::NotDir => Errno::ENOTDIR,
        FsError::NotEmpty => Errno::ENOTEMPTY,
        FsError::BadHandle | FsError::ReadOnly => Errno::EBADF,
        FsError::NoSpace => Errno::ENOSPC,
        FsError::Stale => Errno::ESTALE,
        FsError::InvalidPath => Errno::EINVAL,
        FsError::Io(error) => {
            eprintln!("RPool FUSE I/O error: {error:#}");
            Errno::EIO
        }
    }
}
