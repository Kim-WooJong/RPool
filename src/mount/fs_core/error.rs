//! Filesystem errors that every frontend maps to its own status codes.
use super::super::spool::SpoolBudgetExceeded;

#[derive(Debug)]
/// Error of an `FsCore` operation; each frontend maps it to errno or NTSTATUS
/// (see `frontend::fuse::errno`).
pub(crate) enum FsError {
    /// No such file or directory.
    NotFound,
    /// Target already exists.
    Exists,
    /// Is a directory where a file is required.
    IsDir,
    /// A path component or target is not a directory.
    NotDir,
    /// Directory is not empty.
    NotEmpty,
    /// Unknown or closed handle.
    BadHandle,
    /// Write through a read-only handle.
    ReadOnly,
    /// The local write spool budget is exhausted; acknowledged data is kept.
    NoSpace,
    /// The file was unlinked or replaced; unsealed writes cannot be acknowledged.
    Stale,
    /// Path is not a valid drive path.
    InvalidPath,
    /// Any other failure (drive, I/O, poisoned lock), with its cause.
    Io(anyhow::Error),
}
/// Result of an `FsCore` operation.
pub(crate) type FsResult<T> = Result<T, FsError>;

impl From<anyhow::Error> for FsError {
    fn from(error: anyhow::Error) -> Self {
        if error.chain().any(|cause| cause.is::<SpoolBudgetExceeded>()) {
            return Self::NoSpace;
        }
        Self::Io(error)
    }
}
impl From<std::io::Error> for FsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.into())
    }
}
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error:#}"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for FsError {}
