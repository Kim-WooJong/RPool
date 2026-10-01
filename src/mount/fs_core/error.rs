//! Filesystem errors that every frontend maps to its own status codes.
use super::super::spool::SpoolBudgetExceeded;

#[derive(Debug)]
pub(crate) enum FsError {
    NotFound,
    Exists,
    IsDir,
    NotDir,
    NotEmpty,
    BadHandle,
    ReadOnly,
    /// The local write spool budget is exhausted; acknowledged data is kept.
    NoSpace,
    /// The file was unlinked or replaced; unsealed writes cannot be acknowledged.
    Stale,
    InvalidPath,
    Io(anyhow::Error),
}
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
