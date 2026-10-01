//! Isolated synthetic Unix filesystem backend, never production archive routing.
//! Owns a fresh private TempDir. All subsequent resolution is descriptor-relative
//! and no-follow. Requires exclusive ownership: malicious same-UID directory
//! relocation/mutation is outside this prototype's isolation guarantee.
//! Windows/reparse-point support is intentionally not compiled or advertised.

#[cfg(test)]
mod tests;

use crate::storage::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use rustix::io::Errno;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};

const CHUNK: usize = 64 * 1024;
const STAGING_PREFIX: &str = ".rpool-stage-";

pub(crate) struct LocalBackend {
    id: BackendId,
    root: File,
    // Keep the directory alive longer than its handle; only this backend owns it.
    directory: tempfile::TempDir,
    sequence: AtomicU64,
}

fn check(ctx: &OperationContext) -> Result<(), StorageError> {
    if ctx.is_cancelled() {
        return Err(StorageError::Cancelled {
            detail: "local operation cancelled".into(),
        });
    }
    if ctx.deadline_passed() {
        return Err(StorageError::Timeout {
            detail: "local operation deadline elapsed".into(),
        });
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> StorageError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => StorageError::not_found("local object"),
        ErrorKind::AlreadyExists => StorageError::AlreadyExists {
            path: "local object".into(),
        },
        ErrorKind::PermissionDenied => StorageError::PermissionDenied {
            path: "local object".into(),
        },
        ErrorKind::InvalidInput | ErrorKind::NotADirectory | ErrorKind::IsADirectory => {
            StorageError::invalid_input("invalid local object path")
        }
        _ if error.raw_os_error() == Some(Errno::LOOP.raw_os_error()) => {
            StorageError::invalid_input("local symlink forbidden")
        }
        _ => StorageError::TransientIo {
            detail: "local filesystem I/O failed".into(),
        },
    }
}
fn os_error(error: Errno) -> StorageError {
    io_error(error.into())
}

/// Cleanup relative to the retained parent, including every early return.
struct Stage<'a> {
    parent: &'a File,
    name: String,
    file: File,
}
impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = fs::unlinkat(self.parent, &self.name, AtFlags::empty());
    }
}

impl LocalBackend {
    /// No arbitrary directory constructor: only fresh synthetic data is allowed.
    pub(crate) fn new(id: BackendId) -> Result<Self, StorageError> {
        let directory = tempfile::Builder::new()
            .prefix("rpool-local-")
            .tempdir()
            .map_err(io_error)?;
        let root = File::from(
            fs::open(
                directory.path(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(os_error)?,
        );
        Ok(Self {
            id,
            root,
            directory,
            sequence: AtomicU64::new(0),
        })
    }

    fn parent(&self, key: &ObjectKey, create: bool) -> Result<(File, String), StorageError> {
        let parts: Vec<_> = key.as_str().split('/').collect();
        // Reject platform aliases and reserve staging names; never normalize keys.
        if parts.iter().any(|p| {
            p.is_empty()
                || *p == "."
                || *p == ".."
                || p.contains(':')
                || p.starts_with(STAGING_PREFIX)
                || p.bytes().any(|b| b < 32 || b == 127)
        }) {
            return Err(StorageError::invalid_input("invalid local key component"));
        }
        let mut directory = self.root.try_clone().map_err(io_error)?;
        for component in &parts[..parts.len() - 1] {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let opened = match fs::openat(&directory, *component, flags, Mode::empty()) {
                Err(Errno::NOENT) if create => {
                    match fs::mkdirat(&directory, *component, Mode::RWXU) {
                        Ok(()) | Err(Errno::EXIST) => (),
                        Err(error) => return Err(os_error(error)),
                    }
                    fs::openat(&directory, *component, flags, Mode::empty())
                }
                other => other,
            }
            .map_err(os_error)?;
            directory = File::from(opened);
        }
        Ok((directory, parts.last().unwrap().to_string()))
    }

    fn open_object(&self, key: &ObjectKey) -> Result<File, StorageError> {
        let (parent, leaf) = self.parent(key, false)?;
        let file = File::from(
            fs::openat(
                &parent,
                leaf,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(os_error)?,
        );
        if !file.metadata().map_err(io_error)?.is_file() {
            return Err(StorageError::invalid_input(
                "local object is not a regular file",
            ));
        }
        Ok(file)
    }

    fn validate_destination(parent: &File, leaf: &str) -> Result<(), StorageError> {
        match fs::statat(parent, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::RegularFile => {
                Ok(())
            }
            Ok(_) => Err(StorageError::invalid_input(
                "local destination is not a regular file",
            )),
            Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(os_error(error)),
        }
    }

    fn stage<'a>(&self, parent: &'a File) -> Result<Stage<'a>, StorageError> {
        let sequence = self
            .sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| StorageError::Other {
                detail: "local staging sequence exhausted".into(),
            })?;
        let name = format!("{STAGING_PREFIX}{sequence}");
        let file = File::from(
            fs::openat(
                parent,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(os_error)?,
        );
        Ok(Stage { parent, name, file })
    }
}

impl StorageBackend for LocalBackend {
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
            conditional_update: Unsupported,
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
        check(ctx)?;
        let metadata = self.open_object(key)?.metadata().map_err(io_error)?;
        check(ctx)?;
        Ok(ObjectMetadata {
            size: metadata.len(),
            is_dir: false,
            version: None,
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
        check(ctx)?;
        let mut file = self.open_object(key)?;
        let size = file.metadata().map_err(io_error)?.len();
        let offset = range.offset().min(size);
        let mut remaining = range.end().min(size) - offset;
        // Do not seek a huge requested offset past EOF.
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut buffer = [0u8; CHUNK];
        let mut bytes_read = 0;
        while remaining > 0 {
            check(ctx)?;
            let count = remaining.min(CHUNK as u64) as usize;
            let n = match file.read(&mut buffer[..count]) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.map_err(io_error)?,
            };
            if n == 0 {
                break;
            }
            sink.write_all(&buffer[..n]).map_err(io_error)?;
            bytes_read += n as u64;
            remaining -= n as u64;
        }
        check(ctx)?;
        Ok(ReadReceipt {
            bytes_read,
            version: None,
        })
    }
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let mut bytes = Vec::new();
        self.read(
            ctx,
            key,
            &ReadRange::new(0, limit.map(|n| n as u64).unwrap_or(u64::MAX))?,
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
        check(ctx)?;
        if options.expected_version.is_some() {
            return Err(StorageError::unsupported("local conditional update"));
        }
        let (parent, leaf) = self.parent(key, true)?;
        Self::validate_destination(&parent, &leaf)?;
        let mut stage = self.stage(&parent)?;
        let mut buffer = [0u8; CHUNK];
        let mut size = 0u64;
        loop {
            check(ctx)?;
            let n = match source.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.map_err(io_error)?,
            };
            if n == 0 {
                break;
            }
            stage.file.write_all(&buffer[..n]).map_err(io_error)?;
            size = size
                .checked_add(n as u64)
                .ok_or_else(|| StorageError::invalid_input("local object size overflow"))?;
        }
        stage.file.sync_all().map_err(io_error)?;
        check(ctx)?;
        Self::validate_destination(&parent, &leaf)?;
        if options.overwrite {
            fs::renameat(&parent, &stage.name, &parent, &leaf).map_err(os_error)?;
        } else {
            // Atomic no-clobber publication; Drop removes the staging link.
            fs::linkat(&parent, &stage.name, &parent, &leaf, AtFlags::empty()).map_err(os_error)?;
        }
        // File sync != directory-entry power-loss durability. Capability remains false.
        Ok(WriteReceipt {
            size,
            version: None,
        })
    }
    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        check(ctx)?;
        self.open_object(key)?;
        let (parent, leaf) = self.parent(key, false)?;
        check(ctx)?;
        fs::unlinkat(parent, leaf, AtFlags::empty()).map_err(os_error)
    }
    fn list(
        &self,
        ctx: &OperationContext,
        _prefix: &str,
        _page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("local list"))
    }
    fn copy(
        &self,
        ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("local copy"))
    }
    fn rename(
        &self,
        ctx: &OperationContext,
        _source: &ObjectKey,
        _destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        check(ctx)?;
        Err(StorageError::unsupported("local rename"))
    }
}
