//! Synthetic, process-local object store implementing the real storage contract.
//! No filesystem, subprocess, encryption binding, or production routing.
//! Reads clip at EOF; bounded read_all returns a prefix, not proof of completeness.

pub(crate) mod faults;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::storage::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;

const CHUNK: usize = 8192;

#[derive(Clone)]
struct Entry {
    bytes: Arc<[u8]>,
    version: String,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<ObjectKey, Entry>,
    generation: u64,
}

pub(crate) struct MemoryBackend {
    id: BackendId,
    state: Mutex<State>,
}

pub(crate) fn check_context(ctx: &OperationContext) -> Result<(), StorageError> {
    if ctx.is_cancelled() {
        return Err(StorageError::Cancelled {
            detail: "memory operation cancelled".into(),
        });
    }
    if ctx.deadline_passed() {
        return Err(StorageError::Timeout {
            detail: "memory operation deadline elapsed".into(),
        });
    }
    Ok(())
}

fn stream_error() -> StorageError {
    StorageError::TransientIo {
        detail: "memory source/sink I/O failed".into(),
    }
}

impl MemoryBackend {
    pub(crate) fn new(id: BackendId) -> Self {
        Self {
            id,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, StorageError> {
        self.state.lock().map_err(|_| StorageError::Other {
            detail: "memory store lock poisoned".into(),
        })
    }

    fn snapshot(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<Entry, StorageError> {
        check_context(ctx)?;
        let state = self.lock()?;
        check_context(ctx)?;
        state
            .objects
            .get(key)
            .cloned()
            .ok_or_else(|| StorageError::not_found(key.as_str()))
    }
}

impl StorageBackend for MemoryBackend {
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
            conditional_create: Supported,
            conditional_update: Supported,
            conditional_delete: Unsupported,
            version_pinning: Unsupported,
            atomic_replace: Supported,
            durable_after_write: Unsupported,
            consistency_scope: ConsistencyScope::ProcessLocal,
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
        let entry = self.snapshot(ctx, key)?;
        Ok(ObjectMetadata {
            size: entry.bytes.len() as u64,
            is_dir: false,
            version: Some(entry.version),
            modified: None,
        })
    }

    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let entry = self.snapshot(ctx, key)?;
        // Clamp before narrowing, including on 32-bit targets.
        let size = entry.bytes.len() as u64;
        let start = range.offset().min(size) as usize;
        let end = range.end().min(size) as usize;
        for chunk in entry.bytes[start..end].chunks(CHUNK) {
            check_context(ctx)?;
            sink.write_all(chunk).map_err(|_| stream_error())?;
        }
        check_context(ctx)?;
        Ok(ReadReceipt {
            bytes_read: (end - start) as u64,
            version: Some(entry.version),
        })
    }

    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let entry = self.snapshot(ctx, key)?;
        let count = limit.unwrap_or(entry.bytes.len()).min(entry.bytes.len());
        let mut bytes = Vec::new();
        for chunk in entry.bytes[..count].chunks(CHUNK) {
            check_context(ctx)?;
            bytes.extend_from_slice(chunk);
        }
        check_context(ctx)?;
        Ok(bytes)
    }

    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        check_context(ctx)?;
        if !options.overwrite && options.expected_version.is_some() {
            return Err(StorageError::invalid_input(
                "conditional create cannot specify a version",
            ));
        }
        // Never invoke user callbacks while holding the store lock.
        let mut bytes = Vec::new();
        let mut buffer = [0u8; CHUNK];
        loop {
            check_context(ctx)?;
            match source.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(stream_error()),
            }
        }
        let bytes: Arc<[u8]> = bytes.into();
        let mut state = self.lock()?;
        check_context(ctx)?;
        let current = state.objects.get(key);
        if !options.overwrite && current.is_some() {
            return Err(StorageError::AlreadyExists {
                path: key.as_str().into(),
            });
        }
        if let Some(expected) = &options.expected_version {
            if current.map(|entry| &entry.version) != Some(expected) {
                return Err(StorageError::PreconditionFailed {
                    detail: "memory version mismatch".into(),
                });
            }
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| StorageError::Other {
                detail: "memory generation exhausted".into(),
            })?;
        let version = generation.to_string();
        let size = bytes.len() as u64;
        state.objects.insert(
            key.clone(),
            Entry {
                bytes,
                version: version.clone(),
            },
        );
        state.generation = generation;
        // Commit is known to have succeeded: never report a clean cancellation after this point.
        Ok(WriteReceipt {
            size,
            version: Some(version),
            hash_verified: false,
            stored_hash: None,
        })
    }

    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        check_context(ctx)?;
        let mut state = self.lock()?;
        check_context(ctx)?;
        state
            .objects
            .remove(key)
            .map(|_| ())
            .ok_or_else(|| StorageError::not_found(key.as_str()))
    }

    fn list(
        &self,
        ctx: &OperationContext,
        _prefix: &str,
        _page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        check_context(ctx)?;
        Err(StorageError::unsupported("memory list"))
    }

    fn copy(
        &self,
        ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        check_context(ctx)?;
        Err(StorageError::unsupported("memory copy"))
    }

    fn rename(
        &self,
        ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        check_context(ctx)?;
        Err(StorageError::unsupported("memory rename"))
    }
}
