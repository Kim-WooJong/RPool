//! Storage error taxonomy (Step 2 contract).
//!
//! Every backend and transfer path classifies its failures into one of these
//! 14 kinds. Callers must NOT collapse them into `Option::None`, an empty
//! list, or a `Missing` sentinel (target.md §3.5, R1). An ambiguous remote
//! outcome is `UnknownOutcome`, never guessed as `NotFound`.
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

use std::fmt;
use std::time::Duration;

/// Stable classification used by callers that must branch on error kind without
/// matching the full variant (retry policy, repair policy, resume policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StorageErrorKind {
    NotFound,
    AlreadyExists,
    PermissionDenied,
    Authentication,
    RateLimited,
    Timeout,
    Cancelled,
    InvalidInput,
    Unsupported,
    PreconditionFailed,
    CorruptData,
    TransientIo,
    UnknownOutcome,
    Other,
}

/// Typed storage error. Carries only non-secret, non-command detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StorageError {
    NotFound { path: String },
    AlreadyExists { path: String },
    PermissionDenied { path: String },
    Authentication { detail: String },
    RateLimited {
        retry_after: Option<Duration>,
        detail: String,
    },
    Timeout { detail: String },
    Cancelled { detail: String },
    InvalidInput { detail: String },
    Unsupported { operation: String },
    PreconditionFailed { detail: String },
    CorruptData { found: String, expected: String },
    TransientIo { detail: String },
    /// A remote write whose response was lost. This is NOT "nothing happened":
    /// the write may already have committed. It must not be retried blindly.
    UnknownOutcome { detail: String },
    Other { detail: String },
}

impl StorageError {
    pub(crate) fn kind(&self) -> StorageErrorKind {
        match self {
            Self::NotFound { .. } => StorageErrorKind::NotFound,
            Self::AlreadyExists { .. } => StorageErrorKind::AlreadyExists,
            Self::PermissionDenied { .. } => StorageErrorKind::PermissionDenied,
            Self::Authentication { .. } => StorageErrorKind::Authentication,
            Self::RateLimited { .. } => StorageErrorKind::RateLimited,
            Self::Timeout { .. } => StorageErrorKind::Timeout,
            Self::Cancelled { .. } => StorageErrorKind::Cancelled,
            Self::InvalidInput { .. } => StorageErrorKind::InvalidInput,
            Self::Unsupported { .. } => StorageErrorKind::Unsupported,
            Self::PreconditionFailed { .. } => StorageErrorKind::PreconditionFailed,
            Self::CorruptData { .. } => StorageErrorKind::CorruptData,
            Self::TransientIo { .. } => StorageErrorKind::TransientIo,
            Self::UnknownOutcome { .. } => StorageErrorKind::UnknownOutcome,
            Self::Other { .. } => StorageErrorKind::Other,
        }
    }

    /// Whether a retry is meaningful for this kind. `UnknownOutcome` is NOT
    /// retriable: a lost response may already have committed the write.
    pub(crate) fn is_retriable(&self) -> bool {
        matches!(
            self.kind(),
            StorageErrorKind::RateLimited
                | StorageErrorKind::Timeout
                | StorageErrorKind::TransientIo
        )
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub(crate) fn not_found(path: impl Into<String>) -> Self {
        Self::NotFound { path: path.into() }
    }

    pub(crate) fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported { operation: operation.into() }
    }

    pub(crate) fn invalid_input(detail: impl Into<String>) -> Self {
        Self::InvalidInput { detail: detail.into() }
    }

    pub(crate) fn unknown_outcome(detail: impl Into<String>) -> Self {
        Self::UnknownOutcome { detail: detail.into() }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { path } => write!(f, "not found: {path}"),
            Self::AlreadyExists { path } => write!(f, "already exists: {path}"),
            Self::PermissionDenied { path } => write!(f, "permission denied: {path}"),
            Self::Authentication { detail } => write!(f, "authentication failed: {detail}"),
            Self::RateLimited { retry_after, detail } => match retry_after {
                Some(d) => write!(f, "rate limited (retry after {d:?}): {detail}"),
                None => write!(f, "rate limited: {detail}"),
            },
            Self::Timeout { detail } => write!(f, "timeout: {detail}"),
            Self::Cancelled { detail } => write!(f, "cancelled: {detail}"),
            Self::InvalidInput { detail } => write!(f, "invalid input: {detail}"),
            Self::Unsupported { operation } => write!(f, "unsupported operation: {operation}"),
            Self::PreconditionFailed { detail } => write!(f, "precondition failed: {detail}"),
            Self::CorruptData { found, expected } => {
                write!(f, "corrupt data: found {found}, expected {expected}")
            }
            Self::TransientIo { detail } => write!(f, "transient I/O error: {detail}"),
            Self::UnknownOutcome { detail } => write!(f, "unknown outcome: {detail}"),
            Self::Other { detail } => write!(f, "storage error: {detail}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_classification_covers_all_variants() {
        assert_eq!(StorageError::not_found("x").kind(), StorageErrorKind::NotFound);
        assert_eq!(
            StorageError::AlreadyExists { path: "x".into() }.kind(),
            StorageErrorKind::AlreadyExists
        );
        assert_eq!(
            StorageError::PermissionDenied { path: "x".into() }.kind(),
            StorageErrorKind::PermissionDenied
        );
        assert_eq!(
            StorageError::Authentication { detail: "d".into() }.kind(),
            StorageErrorKind::Authentication
        );
        assert_eq!(
            StorageError::RateLimited { retry_after: None, detail: "d".into() }.kind(),
            StorageErrorKind::RateLimited
        );
        assert_eq!(
            StorageError::Timeout { detail: "d".into() }.kind(),
            StorageErrorKind::Timeout
        );
        assert_eq!(
            StorageError::Cancelled { detail: "d".into() }.kind(),
            StorageErrorKind::Cancelled
        );
        assert_eq!(StorageError::invalid_input("d").kind(), StorageErrorKind::InvalidInput);
        assert_eq!(StorageError::unsupported("op").kind(), StorageErrorKind::Unsupported);
        assert_eq!(
            StorageError::PreconditionFailed { detail: "d".into() }.kind(),
            StorageErrorKind::PreconditionFailed
        );
        assert_eq!(
            StorageError::CorruptData { found: "a".into(), expected: "b".into() }.kind(),
            StorageErrorKind::CorruptData
        );
        assert_eq!(
            StorageError::TransientIo { detail: "d".into() }.kind(),
            StorageErrorKind::TransientIo
        );
        assert_eq!(
            StorageError::unknown_outcome("d").kind(),
            StorageErrorKind::UnknownOutcome
        );
        assert_eq!(
            StorageError::Other { detail: "d".into() }.kind(),
            StorageErrorKind::Other
        );
    }

    #[test]
    fn retriable_classification() {
        assert!(StorageError::RateLimited { retry_after: None, detail: "d".into() }.is_retriable());
        assert!(StorageError::Timeout { detail: "d".into() }.is_retriable());
        assert!(StorageError::TransientIo { detail: "d".into() }.is_retriable());
        // UnknownOutcome is NOT retriable: a lost response may have committed.
        assert!(!StorageError::unknown_outcome("d").is_retriable());
        assert!(!StorageError::not_found("x").is_retriable());
        assert!(!StorageError::unsupported("op").is_retriable());
        assert!(!StorageError::CorruptData { found: "a".into(), expected: "b".into() }.is_retriable());
    }

    #[test]
    fn retry_after_is_preserved_only_for_rate_limited() {
        let e = StorageError::RateLimited {
            retry_after: Some(Duration::from_secs(5)),
            detail: "slow down".into(),
        };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(5)));
        assert_eq!(StorageError::not_found("x").retry_after(), None);
        assert_eq!(
            StorageError::RateLimited { retry_after: None, detail: "d".into() }.retry_after(),
            None
        );
    }

    #[test]
    fn display_and_error_trait_conversion() {
        let e = StorageError::CorruptData { found: "abc".into(), expected: "def".into() };
        let rendered = format!("{e}");
        assert!(rendered.contains("corrupt"));
        // Implements std::error::Error + Send + Sync, so it converts to anyhow::Error.
        let _anyhow: anyhow::Error = e.into();
    }
}
