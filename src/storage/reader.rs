//! Backend-neutral verified read service. Manifest strings are lookup keys only;
//! runtime bindings never alter or enter the persisted manifest representation.
use super::error::{StorageError, StorageErrorKind};
use super::rclone::{RcloneBackend, RcloneContext};
use super::reference::{BackendId, ObjectKey, ObjectRef};
use super::registry::BackendRegistry;
use super::traits::{ObjectMetadata, OperationContext, ReadRange, StorageBackend};
use crate::prelude::*;
use crate::utils::write_all_at;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(crate) struct ReadProgress {
    started: Instant,
    bytes: AtomicU64,
    updated_ms: AtomicU64,
}
impl ReadProgress {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            bytes: AtomicU64::new(0),
            updated_ms: AtomicU64::new(0),
        }
    }
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    pub(crate) fn stalled(&self, delay: Duration) -> bool {
        self.elapsed().saturating_sub(Duration::from_millis(
            self.updated_ms.load(Ordering::Acquire),
        )) >= delay
    }
    pub(crate) fn slow(&self, size: u64, delay: Duration) -> bool {
        let elapsed = self.elapsed();
        let idle = elapsed.saturating_sub(Duration::from_millis(
            self.updated_ms.load(Ordering::Acquire),
        ));
        let bytes = self.bytes.load(Ordering::Acquire);
        idle >= delay
            || (elapsed >= delay
                && bytes > 0
                && bytes < size
                && elapsed.as_secs_f64() * (size - bytes) as f64 / bytes as f64
                    > delay.as_secs_f64())
    }
    fn update(&self, bytes: usize) {
        if bytes != 0 {
            self.bytes.fetch_add(bytes as u64, Ordering::Release);
            self.updated_ms.store(
                self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
        }
    }
}

const METADATA_LIMIT: usize = 64 * 1024 * 1024;
struct Routes {
    registry: BackendRegistry,
    bindings: BTreeMap<String, ObjectRef>,
}

pub(crate) struct StorageReader {
    routes: Mutex<Routes>,
    legacy: Option<RcloneContext>,
    context: OperationContext,
    excluded_remotes: BTreeSet<String>,
}

pub(crate) fn is_recoverable_loss(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<StorageError>().map(StorageError::kind),
        Some(StorageErrorKind::NotFound | StorageErrorKind::CorruptData)
    )
}

/// Read-only availability policy. Never use this to authorize a mutation or
/// invalidate an upload journal: a network outage is not evidence of data loss.
pub(crate) fn is_restore_unavailable(error: &anyhow::Error) -> bool {
    is_recoverable_loss(error)
        || matches!(
            error.downcast_ref::<StorageError>().map(StorageError::kind),
            Some(
                StorageErrorKind::Timeout
                    | StorageErrorKind::ReadExcluded
                    | StorageErrorKind::TransientIo
                    | StorageErrorKind::RateLimited
            )
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
    /// Explicit, read-only recovery policy. Names are remote aliases, never
    /// remote paths. The deadline bounds this reader's entire recovery attempt;
    /// create a fresh reader for each independently resumable file.
    pub(crate) fn rclone_with_excluded_remotes(
        executable: &str,
        excluded: &BTreeSet<String>,
    ) -> Result<Self> {
        validate_excluded_remotes(excluded)?;
        let mut reader = Self::rclone(executable);
        reader.excluded_remotes = excluded.clone();
        reader.context = OperationContext::with_deadline(Instant::now() + Duration::from_secs(120));
        Ok(reader)
    }
    pub(crate) fn with_rclone_context(context: RcloneContext) -> Self {
        Self {
            routes: Mutex::new(Routes {
                registry: BackendRegistry::new(),
                bindings: BTreeMap::new(),
            }),
            legacy: Some(context),
            context: OperationContext::none(),
            excluded_remotes: BTreeSet::new(),
        }
    }
    #[cfg(test)]
    pub(crate) fn from_registry(
        registry: BackendRegistry,
        bindings: BTreeMap<String, ObjectRef>,
        context: OperationContext,
    ) -> Self {
        Self {
            routes: Mutex::new(Routes { registry, bindings }),
            legacy: None,
            context,
            excluded_remotes: BTreeSet::new(),
        }
    }
    pub(super) fn resolve(&self, raw: &str) -> Result<(Arc<dyn StorageBackend>, ObjectKey)> {
        // Check before route lookup, backend construction, or any remote I/O.
        // Match the complete alias, not a path prefix or a similar alias.
        if let Some((remote, _)) = raw.split_once(':') {
            if self.excluded_remotes.contains(remote) {
                return Err(StorageError::ReadExcluded {
                    remote: remote.into(),
                }
                .into());
            }
        }
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
    pub(crate) fn operation_context(&self) -> &OperationContext {
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
        self.verified_read_context(shard, sink, &self.context, None)
    }
    pub(crate) fn download_staged(
        &self,
        shard: &Shard,
        path: &Path,
        context: &OperationContext,
        progress: &ReadProgress,
    ) -> Result<()> {
        let mut file = File::create(path)?;
        let result = self.verified_read_context(shard, &mut file, context, Some(progress));
        drop(file);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }
    fn verified_read_context(
        &self,
        shard: &Shard,
        sink: &mut dyn Write,
        context: &OperationContext,
        progress: Option<&ReadProgress>,
    ) -> Result<()> {
        check_read_context(context)?;
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
            local_error: false,
            sink_cancelled: false,
            progress,
            context,
        };
        let result = backend.read(context, &key, &ReadRange::new(0, length)?, &mut verified);
        if verified.overflow {
            return Err(corrupt("oversized object", format!("size:{}", shard.size)).into());
        }
        if verified.local_error {
            return Err(StorageError::Other {
                detail: "local restore output write failed".into(),
            }
            .into());
        }
        if verified.sink_cancelled
            && result.as_ref().err().is_some_and(|e| {
                matches!(
                    e.kind(),
                    StorageErrorKind::TransientIo
                        | StorageErrorKind::Other
                        | StorageErrorKind::Cancelled
                )
            })
        {
            // Only a cancellation observed by this sink can explain an adapter's
            // generic pipe/sink error. Unrelated fatal adapter errors survive.
            check_read_context(context)?;
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
        check_read_context(context)?;
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

fn validate_excluded_remotes(excluded: &BTreeSet<String>) -> Result<()> {
    for remote in excluded {
        if remote.is_empty()
            || remote == "."
            || remote == ".."
            || remote.starts_with('-')
            || remote.trim() != remote
            || !remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-. ".contains(&b))
        {
            return Err(StorageError::invalid_input(
                "excluded remote must be an alias using letters, digits, underscore, hyphen, dot, or internal ASCII spaces, without a leading hyphen",
            ).into());
        }
    }
    Ok(())
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
    local_error: bool,
    sink_cancelled: bool,
    progress: Option<&'a ReadProgress>,
    context: &'a OperationContext,
}
impl Write for VerifiedSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.context.is_cancelled() || self.context.deadline_passed() {
            self.sink_cancelled = true;
            return Err(std::io::Error::other("read request cancelled"));
        }
        if bytes.len() as u64 > self.size.saturating_sub(self.count) {
            self.overflow = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversized shard",
            ));
        }
        if let Err(error) = self.sink.write_all(bytes) {
            self.local_error = true;
            return Err(error);
        }
        if let Some(progress) = self.progress {
            progress.update(bytes.len());
        }
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

pub(crate) fn check_read_context(context: &OperationContext) -> Result<()> {
    if context.is_cancelled() {
        return Err(StorageError::Cancelled {
            detail: "read cancelled".into(),
        }
        .into());
    }
    if context.deadline_passed() {
        return Err(StorageError::Timeout {
            detail: "read deadline elapsed".into(),
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod recovery_exclusion_tests {
    use super::*;
    use crate::storage::memory::faults::{Fault, FaultBackend};
    use crate::storage::memory::MemoryBackend;
    use crate::storage::traits::WriteOptions;

    #[test]
    fn exclusion_is_explicit_read_only_and_precedes_route_lookup() {
        let excluded = BTreeSet::from(["failed-crypt".into()]);
        let reader =
            StorageReader::rclone_with_excluded_remotes("never-executed", &excluded).unwrap();
        let error = reader.stat("failed-crypt:archive/shard").unwrap_err();
        assert_eq!(
            error.downcast_ref::<StorageError>().unwrap().kind(),
            StorageErrorKind::ReadExcluded
        );
        assert!(is_restore_unavailable(&error));
        assert!(!is_recoverable_loss(&error));
        assert!(!error.downcast_ref::<StorageError>().unwrap().is_retriable());
        assert!(reader.routes.lock().unwrap().bindings.is_empty());
        assert!(reader.operation_context().deadline().is_some());
        // Similar aliases must not be excluded, and resolution itself does not execute rclone.
        assert!(reader.resolve("failed-crypt-other:archive/shard").is_ok());
        assert!(StorageReader::rclone("never-executed")
            .resolve("failed-crypt:archive/shard")
            .is_ok());
        let auth: anyhow::Error = StorageError::Authentication {
            detail: "synthetic".into(),
        }
        .into();
        assert!(!is_restore_unavailable(&auth));
    }

    #[test]
    fn malformed_exclusion_names_are_rejected() {
        for name in [
            "",
            ".",
            "..",
            "bad:",
            "bad:path",
            "bad/path",
            "bad\\path",
            " bad",
            "bad ",
            "bad\nname",
            "-bad",
            "bad\tname",
            "bad@name",
            "bad\u{a0}name",
        ] {
            let result = StorageReader::rclone_with_excluded_remotes(
                "never-executed",
                &BTreeSet::from([name.into()]),
            );
            assert!(result.is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn internal_ascii_spaces_are_exact_remote_aliases() {
        let reader = StorageReader::rclone_with_excluded_remotes(
            "never-executed",
            &BTreeSet::from(["another crypt".into()]),
        )
        .unwrap();
        let error = reader.stat("another crypt:archive/shard").unwrap_err();
        assert_eq!(
            error.downcast_ref::<StorageError>().unwrap().kind(),
            StorageErrorKind::ReadExcluded
        );
        assert!(reader.routes.lock().unwrap().bindings.is_empty());
        assert!(reader.resolve("another crypt extra:archive/shard").is_ok());
        assert!(reader.resolve("another:crypt/archive/shard").is_ok());
    }

    #[test]
    fn excluded_data_shard_restores_from_healthy_parity_without_auth_operation() {
        let id = BackendId::new("recovery-memory").unwrap();
        let memory = Arc::new(MemoryBackend::new(id.clone()));
        let mut blocks = vec![b"ABCD".to_vec(), b"EFGH".to_vec(), vec![0; 4]];
        ReedSolomon::new(2, 1).unwrap().encode(&mut blocks).unwrap();
        let coding = Some(Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 2,
            parity_shards: 1,
            stripe_size: 2,
        });
        let mut bindings = BTreeMap::new();
        let mut shards = Vec::new();
        for (index, bytes) in blocks.iter().enumerate() {
            let key = ObjectKey::new(format!("part-{index}")).unwrap();
            memory
                .write(
                    &OperationContext::none(),
                    &key,
                    &mut std::io::Cursor::new(bytes),
                    &WriteOptions::default(),
                )
                .unwrap();
            let remote = if index == 0 {
                "failed-crypt:"
            } else {
                "healthy-crypt:"
            };
            let object = format!("{remote}archive/part-{index}");
            bindings.insert(object.clone(), ObjectRef::new(id.clone(), key));
            shards.push(Shard {
                index: index as u32,
                offset: if index < 2 { (index * 4) as u64 } else { 0 },
                size: 4,
                remote: remote.into(),
                object,
                blake3: blake3::hash(bytes).to_hex().to_string(),
                kind: if index < 2 {
                    ShardKind::Data
                } else {
                    ShardKind::Parity
                },
                group: 0,
                slot: index as u16,
            });
        }
        let manifest = Manifest {
            version: 2,
            archive_id: "recovery-fixture".into(),
            original_name: "input".into(),
            original_size: 8,
            shard_size: 4,
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(8, 4, &coding, &shards),
            coding,
            shards,
        };
        let backend = Arc::new(FaultBackend::keyed_reads(
            memory,
            vec![(
                ObjectKey::new("part-0").unwrap(),
                Fault::Error(StorageError::Authentication {
                    detail: "excluded backend must never be read".into(),
                }),
            )],
        ));
        let mut registry = BackendRegistry::new();
        registry.register(backend).unwrap();
        let mut reader = StorageReader::from_registry(registry, bindings, OperationContext::none());
        reader.excluded_remotes = BTreeSet::from(["failed-crypt".into()]);
        let temp = tempfile::tempdir().unwrap();
        let metadata = temp.path().join("manifest.json");
        fs::write(&metadata, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let output = temp.path().join("restored");
        crate::commands::get_with_storage(&reader, metadata.to_str().unwrap(), &output, 1, 1)
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), b"ABCDEFGH");
    }
}
