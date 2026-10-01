//! Synthetic OpenDAL Memory adapter. No external Operator/config/root constructor.
//! Runtime and SDK types stay here. Memory is resident; adapter I/O uses 64 KiB chunks.
mod tests;
use super::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use super::error::StorageError;
use super::reference::{BackendId, ObjectKey};
use super::traits::*;
use std::io::{Read, Write};
use std::sync::{Condvar, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

const CHUNK: usize = 64 * 1024;
const OBJECT_LIMIT: u64 = 8 * 1024 * 1024;
pub(crate) struct OpenDalMemory {
    id: BackendId,
    operator: ::opendal::Operator,
    runtime: Option<tokio::runtime::Runtime>,
    owner: Mutex<Option<ThreadId>>,
    available: Condvar,
}
struct Permit<'a>(&'a OpenDalMemory);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut owner) = self.0.owner.lock() {
            *owner = None;
            self.0.available.notify_one();
        }
    }
}
impl Drop for OpenDalMemory {
    fn drop(&mut self) {
        // Safe even if ownership is dropped by an async caller; no blocking runtime destruction.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
impl OpenDalMemory {
    pub(crate) fn new(id: BackendId) -> Result<Self, StorageError> {
        no_nested_runtime()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .map_err(|_| StorageError::Other {
                detail: "prototype runtime initialization failed".into(),
            })?;
        let operator =
            ::opendal::Operator::new(::opendal::services::Memory::default()).map_err(map_error)?;
        Ok(Self {
            id,
            operator,
            runtime: Some(runtime),
            owner: Mutex::new(None),
            available: Condvar::new(),
        })
    }
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("runtime alive during operations")
    }
    fn admit(&self, ctx: &OperationContext) -> Result<Permit<'_>, StorageError> {
        no_nested_runtime()?;
        check(ctx)?;
        let current = std::thread::current().id();
        let mut owner = self.owner.lock().map_err(|_| internal())?;
        while let Some(active) = *owner {
            if active == current {
                return Err(StorageError::invalid_input("reentrant prototype operation"));
            }
            check(ctx)?;
            owner = self
                .available
                .wait_timeout(owner, Duration::from_millis(10))
                .map_err(|_| internal())?
                .0;
        }
        check(ctx)?;
        *owner = Some(current);
        Ok(Permit(self))
    }
    fn metadata(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        let m = self
            .runtime()
            .block_on(self.operator.stat(key.as_str()))
            .map_err(map_error)?;
        Ok(ObjectMetadata {
            size: m.content_length(),
            is_dir: m.is_dir(),
            version: None,
            modified: None,
        })
    }
    fn read_locked(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let metadata = self.metadata(key)?;
        check(ctx)?;
        let end = range.end().min(metadata.size);
        let mut position = range.offset().min(metadata.size);
        let start = position;
        while position < end {
            check(ctx)?;
            let next = (position + CHUNK as u64).min(end);
            let bytes = self
                .runtime()
                .block_on(async {
                    self.operator
                        .read_with(key.as_str())
                        .range(position..next)
                        .await
                })
                .map_err(map_error)?;
            check(ctx)?;
            if bytes.len() as u64 != next - position {
                return Err(StorageError::CorruptData {
                    found: "short prototype read".into(),
                    expected: "complete requested chunk".into(),
                });
            }
            sink.write_all(&bytes.to_vec())
                .map_err(|_| StorageError::Other {
                    detail: "prototype sink failed".into(),
                })?;
            check(ctx)?;
            position = next;
        }
        Ok(ReadReceipt {
            bytes_read: position - start,
            version: None,
        })
    }
}
impl StorageBackend for OpenDalMemory {
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
            durable_after_write: Unsupported,
            consistency_scope: ConsistencyScope::ProcessLocal,
            max_object_size: Some(OBJECT_LIMIT),
            min_part_size: None,
            max_part_size: None,
        }
    }
    fn stat(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        validate_key(key)?;
        let _permit = self.admit(ctx)?;
        let m = self.metadata(key)?;
        check(ctx)?;
        Ok(m)
    }
    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        validate_key(key)?;
        let _permit = self.admit(ctx)?;
        self.read_locked(ctx, key, range, sink)
    }
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        validate_key(key)?;
        let _permit = self.admit(ctx)?;
        let mut bytes = Vec::new();
        self.read_locked(
            ctx,
            key,
            &ReadRange::new(0, limit.map(|n| n as u64).unwrap_or(OBJECT_LIMIT))?,
            &mut bytes,
        )?;
        Ok(bytes)
    }
    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        validate_key(key)?;
        if !options.overwrite || options.expected_version.is_some() {
            return Err(StorageError::unsupported("prototype conditional write"));
        }
        let _permit = self.admit(ctx)?;
        let mut writer = self
            .runtime()
            .block_on(self.operator.writer(key.as_str()))
            .map_err(map_error)?;
        let mut buffer = vec![0; CHUNK];
        let mut size = 0u64;
        // MemoryWriter buffers until close; abort/source failure cannot publish partial bytes.
        let staged = (|| {
            loop {
                check(ctx)?;
                let n = match source.read(&mut buffer) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        return Err(StorageError::Other {
                            detail: "prototype source failed".into(),
                        })
                    }
                };
                check(ctx)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                if size > OBJECT_LIMIT {
                    return Err(StorageError::invalid_input(
                        "prototype object exceeds 8 MiB",
                    ));
                }
                self.runtime()
                    .block_on(writer.write(buffer[..n].to_vec()))
                    .map_err(map_error)?;
            }
            check(ctx)
        })();
        if let Err(error) = staged {
            let _ = self.runtime().block_on(writer.abort());
            return Err(error);
        }
        let receipt = self
            .runtime()
            .block_on(writer.close())
            .map_err(|_| StorageError::unknown_outcome("prototype commit failed"))?;
        if receipt.content_length() != size {
            return Err(StorageError::unknown_outcome(
                "prototype commit receipt mismatch",
            ));
        }
        Ok(WriteReceipt {
            size,
            version: None,
        })
    }
    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        validate_key(key)?;
        let _permit = self.admit(ctx)?;
        self.metadata(key)?;
        check(ctx)?;
        self.runtime()
            .block_on(self.operator.delete(key.as_str()))
            .map_err(|_| StorageError::unknown_outcome("prototype delete failed"))?;
        Ok(())
    }
    fn list(
        &self,
        ctx: &OperationContext,
        _: &str,
        _: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("prototype list"))
    }
    fn copy(
        &self,
        ctx: &OperationContext,
        _: &ObjectKey,
        _: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("prototype native copy"))
    }
    fn rename(
        &self,
        ctx: &OperationContext,
        _: &ObjectKey,
        _: &ObjectKey,
    ) -> Result<(), StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("prototype atomic rename"))
    }
}
fn check(ctx: &OperationContext) -> Result<(), StorageError> {
    if ctx.is_cancelled() {
        return Err(StorageError::Cancelled {
            detail: "prototype operation cancelled".into(),
        });
    }
    if ctx.deadline_passed() {
        return Err(StorageError::Timeout {
            detail: "prototype deadline expired".into(),
        });
    }
    Ok(())
}
fn no_nested_runtime() -> Result<(), StorageError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(StorageError::unsupported(
            "sync prototype called from async runtime",
        ));
    }
    Ok(())
}
fn validate_key(key: &ObjectKey) -> Result<(), StorageError> {
    let raw = key.as_str();
    if raw
        .chars()
        .any(|c| c.is_ascii_control() || c == ':' || c == '\\')
        || raw
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.trim() != s)
    {
        return Err(StorageError::invalid_input(
            "prototype key would be normalized",
        ));
    }
    Ok(())
}
fn internal() -> StorageError {
    StorageError::Other {
        detail: "prototype admission lock failed".into(),
    }
}
fn map_error(error: ::opendal::Error) -> StorageError {
    use ::opendal::ErrorKind as K;
    match error.kind() {
        K::NotFound => StorageError::not_found("prototype object"),
        K::AlreadyExists => StorageError::AlreadyExists {
            path: "prototype object".into(),
        },
        K::PermissionDenied => StorageError::PermissionDenied {
            path: "prototype object".into(),
        },
        K::Unsupported => StorageError::unsupported("prototype operation"),
        K::ConfigInvalid
        | K::IsADirectory
        | K::NotADirectory
        | K::IsSameFile
        | K::RangeNotSatisfied => StorageError::invalid_input("invalid prototype request"),
        K::ConditionNotMatch => StorageError::PreconditionFailed {
            detail: "prototype condition failed".into(),
        },
        K::RateLimited => StorageError::RateLimited {
            retry_after: None,
            detail: "prototype rate limited".into(),
        },
        _ => StorageError::Other {
            detail: "prototype storage error".into(),
        },
    }
}
