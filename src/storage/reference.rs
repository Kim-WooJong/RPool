//! Address and key types (Step 2 contract).
//!
//! `BackendId` / `ObjectKey` / `ObjectRef` are runtime/admin resolution facts.
//! They MUST NOT appear as fields in legacy manifest v1/v2 JSON
//! (compatibility.md §2, ADR-001). Legacy `remote`/`object` strings are
//! preserved exactly: this module never normalizes, URL-decodes, case-folds, or
//! Unicode-normalizes them.
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

use crate::storage::error::StorageError;

/// Shared identifier validation for backend/volume domain IDs.
///
/// Rules: non-empty, ≤ 256 bytes, no NUL, ASCII only, no path separator.
pub(crate) fn validate_identifier(raw: &str, label: &str) -> Result<(), StorageError> {
    if raw.is_empty() {
        return Err(StorageError::invalid_input(format!("{label} cannot be empty")));
    }
    if raw.len() > 256 {
        return Err(StorageError::invalid_input(format!("{label} exceeds 256 bytes")));
    }
    if raw.contains('\0') {
        return Err(StorageError::invalid_input(format!("{label} contains a NUL byte")));
    }
    if !raw.is_ascii() {
        return Err(StorageError::invalid_input(format!("{label} must be ASCII")));
    }
    if raw.contains('/') || raw.contains('\\') {
        return Err(StorageError::invalid_input(format!("{label} cannot contain a path separator")));
    }
    Ok(())
}

/// Detects a Windows drive path (`X:\` or `X:/`) so it is not mis-parsed as a
/// `remote:path` address by splitting on the first colon.
fn looks_like_windows_drive(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(c), Some(':'), Some(sep)) if c.is_ascii_alphabetic() && (sep == '\\' || sep == '/')
    )
}

/// rclone remote names are `[A-Za-z0-9_-]+`.
fn is_valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Stable identifier for a backend instance. Not a manifest field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BackendId(String);

impl BackendId {
    pub(crate) fn new(raw: impl Into<String>) -> Result<Self, StorageError> {
        let raw = raw.into();
        validate_identifier(&raw, "backend id")?;
        Ok(Self(raw))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A key within a backend. Documented rules for NEW native logical keys:
/// separator is `/`; empty, absolute, `..` traversal, and Windows drive paths
/// are rejected. Legacy `object` strings are NOT run through this validator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ObjectKey(String);

impl ObjectKey {
    pub(crate) fn new(raw: impl Into<String>) -> Result<Self, StorageError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(StorageError::invalid_input("object key cannot be empty"));
        }
        if raw.contains('\0') {
            return Err(StorageError::invalid_input("object key contains a NUL byte"));
        }
        if !raw.is_ascii() {
            return Err(StorageError::invalid_input("object key must be ASCII"));
        }
        if raw.contains('\\') {
            return Err(StorageError::invalid_input(
                 "object key cannot contain a backslash",
             ));
         }
        if raw.starts_with('/') {
            return Err(StorageError::invalid_input("object key cannot be absolute"));
        }
        if looks_like_windows_drive(&raw) {
            return Err(StorageError::invalid_input("object key cannot be a Windows drive path"));
        }
        for segment in raw.split('/') {
            if segment == ".." {
                return Err(StorageError::invalid_input(
                    "object key cannot contain '..' traversal",
                ));
            }
        }
        Ok(Self(raw))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// `backend identity + key`. Backend instance creation is the registry's job
/// (composition root), not the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectRef {
    backend: BackendId,
    key: ObjectKey,
}

impl ObjectRef {
    pub(crate) fn new(backend: BackendId, key: ObjectKey) -> Self {
        Self { backend, key }
    }

    pub(crate) fn backend(&self) -> &BackendId {
        &self.backend
    }

    pub(crate) fn key(&self) -> &ObjectKey {
        &self.key
    }
}

/// A parsed legacy `remote:path` address. The exact original string is
/// preserved; no normalization is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyAddress {
    remote: String,
    path: String,
    original: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LegacyAddressError {
    MissingColon,
    EmptyRemote,
    LooksLikeWindowsDrive,
    InvalidRemoteName,
}

impl LegacyAddress {
    /// Parse a legacy `remote:path` string. Splits on the first colon, but a
    /// Windows drive path (`X:\` / `X:/`) is rejected rather than mis-parsed as
    /// `remote=X`, `path=\...`. Colons inside the path (e.g. a Windows path
    /// stored under a remote) are preserved.
    pub(crate) fn parse(raw: &str) -> Result<Self, LegacyAddressError> {
        if looks_like_windows_drive(raw) {
            return Err(LegacyAddressError::LooksLikeWindowsDrive);
        }
        let (remote, path) = raw.split_once(':').ok_or(LegacyAddressError::MissingColon)?;
        if remote.is_empty() {
            return Err(LegacyAddressError::EmptyRemote);
        }
        if !is_valid_remote_name(remote) {
            return Err(LegacyAddressError::InvalidRemoteName);
        }
        Ok(Self {
            remote: remote.to_string(),
            path: path.to_string(),
            original: raw.to_string(),
        })
    }

    pub(crate) fn remote(&self) -> &str {
        &self.remote
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn original(&self) -> &str {
        &self.original
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_address_round_trip_preserves_exact_string() {
        let addr = LegacyAddress::parse("mycrypt:shards/00000001").unwrap();
        assert_eq!(addr.remote(), "mycrypt");
        assert_eq!(addr.path(), "shards/00000001");
        assert_eq!(addr.original(), "mycrypt:shards/00000001");
    }

    #[test]
    fn legacy_address_preserves_colons_inside_path() {
        // A Windows-style path stored under a remote keeps its colon.
        let addr = LegacyAddress::parse("myremote:C:\\Users\\foo").unwrap();
        assert_eq!(addr.remote(), "myremote");
        assert_eq!(addr.path(), "C:\\Users\\foo");
        assert_eq!(addr.original(), "myremote:C:\\Users\\foo");
    }

    #[test]
    fn legacy_address_rejects_windows_drive() {
        assert_eq!(
            LegacyAddress::parse("C:\\Users\\foo").unwrap_err(),
            LegacyAddressError::LooksLikeWindowsDrive
        );
        assert_eq!(
            LegacyAddress::parse("C:/Users/foo").unwrap_err(),
            LegacyAddressError::LooksLikeWindowsDrive
        );
    }

    #[test]
    fn legacy_address_rejects_empty_remote() {
        assert_eq!(
            LegacyAddress::parse(":foo").unwrap_err(),
            LegacyAddressError::EmptyRemote
        );
    }

    #[test]
    fn legacy_address_rejects_missing_colon() {
        assert_eq!(
            LegacyAddress::parse("foo").unwrap_err(),
            LegacyAddressError::MissingColon
        );
    }

    #[test]
    fn legacy_address_rejects_invalid_remote_name() {
        assert_eq!(
            LegacyAddress::parse("my remote:foo").unwrap_err(),
            LegacyAddressError::InvalidRemoteName
        );
    }

    #[test]
    fn backend_id_validation() {
        assert!(BackendId::new("").is_err());
        assert!(BackendId::new("a/b").is_err());
        assert!(BackendId::new("a\\b").is_err());
        assert!(BackendId::new("mem").is_ok());
        assert_eq!(BackendId::new("mem").unwrap().as_str(), "mem");
    }

    #[test]
    fn object_key_validation() {
        assert!(ObjectKey::new("").is_err());
        assert!(ObjectKey::new("/abs").is_err());
        assert!(ObjectKey::new("../a").is_err());
        assert!(ObjectKey::new("a/../b").is_err());
        assert!(ObjectKey::new("C:\\foo").is_err());
        // Windows rooted / UNC absolute paths must be rejected: `/` is the only
        // valid separator, so any backslash is invalid.
        assert!(ObjectKey::new("\\foo").is_err());
        assert!(ObjectKey::new("\\\\server\\share").is_err());
        assert!(ObjectKey::new("a/b/c").is_ok());
        assert_eq!(ObjectKey::new("a/b/c").unwrap().as_str(), "a/b/c");
    }

    #[test]
    fn object_ref_combines_backend_and_key() {
        let reference = ObjectRef::new(
            BackendId::new("mem").unwrap(),
            ObjectKey::new("a/b").unwrap(),
        );
        assert_eq!(reference.backend().as_str(), "mem");
        assert_eq!(reference.key().as_str(), "a/b");
    }
}
