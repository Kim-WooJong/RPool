//! `FsError` → NTSTATUS.
use crate::mount::fs_core::FsError;
use winfsp_wrs::{
    NTSTATUS, STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY, STATUS_DISK_FULL,
    STATUS_FILE_DELETED, STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_HANDLE, STATUS_IO_DEVICE_ERROR,
    STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_INVALID,
    STATUS_OBJECT_NAME_NOT_FOUND,
};

pub(super) fn status(error: FsError) -> NTSTATUS {
    match error {
        FsError::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        FsError::Exists => STATUS_OBJECT_NAME_COLLISION,
        FsError::IsDir => STATUS_FILE_IS_A_DIRECTORY,
        FsError::NotDir => STATUS_NOT_A_DIRECTORY,
        FsError::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        FsError::BadHandle => STATUS_INVALID_HANDLE,
        FsError::ReadOnly => STATUS_ACCESS_DENIED,
        FsError::NoSpace => STATUS_DISK_FULL,
        FsError::Stale => STATUS_FILE_DELETED,
        FsError::InvalidPath => STATUS_OBJECT_NAME_INVALID,
        FsError::Io(error) => {
            eprintln!("RPool WinFsp I/O error: {error:#}");
            STATUS_IO_DEVICE_ERROR
        }
    }
}
