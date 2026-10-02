//! `StorageBackend` implementation over an rclone remote.

use super::*;

/// `StorageBackend` that maps object keys to addresses under an rclone remote
/// root and performs I/O via rclone (daemon or subprocess).
pub(crate) struct RcloneBackend {
    /// Identifier reported by `id()`.
    pub(super) id: BackendId,
    /// rclone binary/config/environment used for every call.
    pub(super) context: RcloneContext,
    /// Remote root that keys are appended to (e.g. `remote:` or `remote:dir`); empty for legacy bindings.
    pub(super) root: String,
    /// Raw legacy address bound to the single key `legacy-object`; `None` for normal backends.
    pub(super) legacy_object: Option<String>,
    /// Base remote of a native crypt backend: writes carry RPool ciphertext.
    pub(super) crypt_base: bool,
}

impl RcloneBackend {
    #[cfg(test)]
    pub(crate) fn new(
        id: BackendId,
        context: RcloneContext,
        root: String,
    ) -> Result<Self, StorageError> {
        remote_name(&root)?;
        Ok(Self {
            id,
            context,
            root,
            legacy_object: None,
            crypt_base: false,
        })
    }
    /// Base remote under a native `CryptBackend`. The token can only be created by
    /// the native crypt router, after it validated the crypt remote and its base.
    pub(crate) fn for_crypt_base(
        id: BackendId,
        context: RcloneContext,
        root: String,
        _access: crate::storage::native_crypt::route::BaseAccess,
    ) -> Result<Self, StorageError> {
        remote_name(&root)?;
        Ok(Self {
            id,
            context,
            root,
            legacy_object: None,
            crypt_base: true,
        })
    }
    /// Runtime-only binding. The safe key never contains or normalizes the legacy address.
    pub(crate) fn for_legacy_object(id: BackendId, context: RcloneContext, raw: String) -> Self {
        Self {
            id,
            context,
            root: String::new(),
            legacy_object: Some(raw),
            crypt_base: false,
        }
    }
    /// Full rclone address for `key` (`root` + separator + key); for a legacy
    /// binding only `legacy-object` resolves, anything else is `NotFound`.
    pub(super) fn address(&self, key: &ObjectKey) -> Result<String, StorageError> {
        if let Some(raw) = &self.legacy_object {
            return if key.as_str() == "legacy-object" {
                Ok(raw.clone())
            } else {
                Err(StorageError::not_found("unbound legacy key"))
            };
        }
        Ok(format!(
            "{}{}{}",
            self.root,
            if self.root.ends_with([':', '/']) {
                ""
            } else {
                "/"
            },
            key.as_str()
        ))
    }
}

impl StorageBackend for RcloneBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }
    fn capabilities(&self) -> BackendCapabilities {
        use Capability::{Supported, Unsupported};
        BackendCapabilities {
            read: Supported,
            ranged_read: Supported,
            streaming_read: Supported,
            write: Supported,
            overwrite: Supported,
            delete: Supported,
            list: Unsupported,
            copy_same_backend: Unsupported,
            rename: Unsupported,
            conditional_create: Unsupported,
            conditional_update: Unsupported,
            conditional_delete: Unsupported,
            version_pinning: Unsupported,
            atomic_replace: Capability::Unknown,
            durable_after_write: Capability::Unknown,
            consistency_scope: ConsistencyScope::Unknown,
            max_object_size: None,
            min_part_size: None,
            max_part_size: None,
        }
    }
    fn stat(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.context.stat_raw(ctx, &self.address(key)?)
    }
    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        self.context
            .read_raw(ctx, &self.address(key)?, Some(range), sink)
    }
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        self.context.read_all_raw(ctx, &self.address(key)?, limit)
    }
    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        let address = self.address(key)?;
        if self.crypt_base {
            self.context
                .write_ungated(ctx, &address, source, None, options)
        } else {
            self.context.write_raw(ctx, &address, source, None, options)
        }
    }
    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        self.context.delete_raw(ctx, &self.address(key)?)
    }
    fn list(
        &self,
        _ctx: &OperationContext,
        _prefix: &str,
        _page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        Err(StorageError::unsupported("rclone bounded listing"))
    }
    fn copy(
        &self,
        _ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        Err(StorageError::unsupported("rclone guaranteed native copy"))
    }
    fn rename(
        &self,
        _ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        Err(StorageError::unsupported("rclone atomic rename"))
    }
}
