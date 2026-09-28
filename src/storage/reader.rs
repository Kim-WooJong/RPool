//! Backend-neutral verified read service. Manifest strings are lookup keys only;
//! runtime bindings never alter or enter the persisted manifest representation.
use super::error::{StorageError, StorageErrorKind};
use super::rclone::{RcloneBackend, RcloneContext};
use super::reference::{BackendId, ObjectKey, ObjectRef};
use super::registry::BackendRegistry;
use super::traits::{ObjectMetadata, OperationContext, ReadRange, StorageBackend};
use crate::prelude::*;
use crate::utils::write_all_at;

const METADATA_LIMIT: usize = 64 * 1024 * 1024;
struct Routes {
    registry: BackendRegistry,
    bindings: BTreeMap<String, ObjectRef>,
}

pub(crate) struct StorageReader {
    routes: Mutex<Routes>,
    legacy: Option<RcloneContext>,
    context: OperationContext,
}

pub(crate) fn is_recoverable_loss(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<StorageError>().map(StorageError::kind),
        Some(StorageErrorKind::NotFound | StorageErrorKind::CorruptData)
    )
}

fn corrupt(found: impl Into<String>, expected: impl Into<String>) -> StorageError {
    StorageError::CorruptData {
        found: found.into(),
        expected: expected.into(),
    }
}

impl StorageReader {
    pub(crate) fn rclone(executable: &str) -> Self {
        Self::with_rclone_context(RcloneContext::inherited(executable))
    }
    pub(crate) fn with_rclone_context(context: RcloneContext) -> Self {
        Self {
            routes: Mutex::new(Routes {
                registry: BackendRegistry::new(),
                bindings: BTreeMap::new(),
            }),
            legacy: Some(context),
            context: OperationContext::none(),
        }
    }
    pub(crate) fn from_registry(
        registry: BackendRegistry,
        bindings: BTreeMap<String, ObjectRef>,
        context: OperationContext,
    ) -> Self {
        Self {
            routes: Mutex::new(Routes { registry, bindings }),
            legacy: None,
            context,
        }
    }
    pub(super) fn resolve(&self, raw: &str) -> Result<(Arc<dyn StorageBackend>, ObjectKey)> {
        let mut routes = self.routes.lock().map_err(|_| StorageError::Other {
            detail: "read route lock poisoned".into(),
        })?;
        if !routes.bindings.contains_key(raw) {
            let context = self
                .legacy
                .as_ref()
                .ok_or_else(|| StorageError::invalid_input("unbound read address"))?;
            let id = BackendId::new(format!("legacy-read-{}", routes.bindings.len()))?;
            let key = ObjectKey::new("legacy-object")?;
            let backend = Arc::new(RcloneBackend::for_legacy_object(
                id.clone(),
                context.clone(),
                raw.to_owned(),
            ));
            routes.registry.register(backend)?;
            routes
                .bindings
                .insert(raw.to_owned(), ObjectRef::new(id, key));
        }
        let reference = routes.bindings.get(raw).expect("binding inserted");
        Ok((
            routes
                .registry
                .resolve(reference)
                .map_err(|_| StorageError::invalid_input("read backend is not registered"))?
                .clone(),
            reference.key().clone(),
        ))
    }
    pub(super) fn operation_context(&self) -> &OperationContext {
        &self.context
    }
    pub(super) fn legacy_context(&self) -> Option<&RcloneContext> {
        self.legacy.as_ref()
    }
    pub(crate) fn stat(&self, raw: &str) -> Result<ObjectMetadata> {
        let (backend, key) = self.resolve(raw)?;
        let metadata = backend.stat(&self.context, &key)?;
        if metadata.is_dir {
            return Err(StorageError::invalid_input("expected object, found directory").into());
        }
        Ok(metadata)
    }
    pub(crate) fn read_metadata(&self, raw: &str) -> Result<Vec<u8>> {
        let (backend, key) = self.resolve(raw)?;
        let metadata = backend.stat(&self.context, &key)?;
        if metadata.is_dir || metadata.size > METADATA_LIMIT as u64 {
            return Err(StorageError::invalid_input(
                "metadata object exceeds bounds or is a directory",
            )
            .into());
        }
        let bytes = backend.read_all(&self.context, &key, Some(METADATA_LIMIT + 1))?;
        if bytes.len() > METADATA_LIMIT {
            return Err(StorageError::invalid_input("metadata object exceeds bounds").into());
        }
        if bytes.len() as u64 != metadata.size {
            return Err(corrupt(bytes.len().to_string(), metadata.size.to_string()).into());
        }
        Ok(bytes)
    }
    pub(crate) fn verify(&self, shard: &Shard, full: bool) -> Result<()> {
        let size = self.stat(&shard.object)?.size;
        if size != shard.size {
            return Err(corrupt(format!("size:{size}"), format!("size:{}", shard.size)).into());
        }
        if full {
            self.verified_read(shard, &mut std::io::sink())?;
        }
        Ok(())
    }
    pub(crate) fn probe(&self, shard: &Shard, full: bool) -> Probe {
        match self.stat(&shard.object) {
            Ok(metadata) if metadata.size != shard.size => Probe::BadSize {
                found: metadata.size,
                expected: shard.size,
            },
            Ok(_) if !full => Probe::Ok,
            Ok(_) => match self.verified_read(shard, &mut std::io::sink()) {
                Ok(()) => Probe::Ok,
                Err(error) => probe_error(error),
            },
            Err(error) => probe_error(error),
        }
    }
    pub(crate) fn verified_read(&self, shard: &Shard, sink: &mut dyn Write) -> Result<()> {
        let (backend, key) = self.resolve(&shard.object)?;
        let length = shard
            .size
            .checked_add(1)
            .ok_or_else(|| StorageError::invalid_input("shard verification range overflow"))?;
        let mut verified = VerifiedSink {
            sink,
            hash: Hasher::new(),
            count: 0,
            size: shard.size,
            overflow: false,
        };
        let result = backend.read(
            &self.context,
            &key,
            &ReadRange::new(0, length)?,
            &mut verified,
        );
        if verified.overflow {
            return Err(corrupt("oversized object", format!("size:{}", shard.size)).into());
        }
        let receipt = result?;
        if verified.count != shard.size || receipt.bytes_read != verified.count {
            return Err(corrupt(
                format!("size:{} receipt:{}", verified.count, receipt.bytes_read),
                format!("size:{}", shard.size),
            )
            .into());
        }
        let hash = verified.hash.finalize().to_hex().to_string();
        if hash != shard.blake3 {
            return Err(corrupt(hash, shard.blake3.clone()).into());
        }
        verified.flush()?;
        Ok(())
    }
    pub(crate) fn download(
        &self,
        shard: &Shard,
        path: &Path,
        retries: u32,
        at_offset: bool,
    ) -> Result<()> {
        for attempt in 1..=retries.max(1) {
            let result = if at_offset {
                let file = OpenOptions::new().read(true).write(true).open(path)?;
                self.verified_read(
                    shard,
                    &mut OffsetSink {
                        file,
                        position: shard.offset,
                    },
                )
            } else {
                let mut file = File::create(path)?;
                let result = self.verified_read(shard, &mut file);
                drop(file);
                if result.is_err() {
                    let _ = fs::remove_file(path);
                }
                result
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let retryable = error
                        .downcast_ref::<StorageError>()
                        .is_some_and(StorageError::is_retriable);
                    if !retryable || attempt == retries.max(1) {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!()
    }
}

fn probe_error(error: anyhow::Error) -> Probe {
    match error.downcast_ref::<StorageError>() {
        Some(StorageError::NotFound { .. }) => Probe::Missing,
        Some(StorageError::CorruptData { found, expected }) => Probe::Corrupt {
            found: found.clone(),
            expected: expected.clone(),
        },
        _ => Probe::Error(format!("{error:#}")),
    }
}
struct VerifiedSink<'a> {
    sink: &'a mut dyn Write,
    hash: Hasher,
    count: u64,
    size: u64,
    overflow: bool,
}
impl Write for VerifiedSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.size.saturating_sub(self.count) {
            self.overflow = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversized shard",
            ));
        }
        self.sink.write_all(bytes)?;
        self.hash.update(bytes);
        self.count += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.sink.flush()
    }
}
struct OffsetSink {
    file: File,
    position: u64,
}
impl Write for OffsetSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .position
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;
        write_all_at(&self.file, bytes, self.position).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Other, "restore output write failed")
        })?;
        self.position = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
