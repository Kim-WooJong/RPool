//! Gated object writes, deletes and mutation retry/permit handling.

use super::*;

impl RcloneContext {
    /// Uploads `source` to a crypt remote address with `rclone rcat` (rclone
    /// encrypts). Refuses conditional writes and non-crypt destinations (`ensure_crypt`).
    /// Used by `RcloneBackend::write` and the speed test.
    pub(crate) fn write_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        source: &mut dyn Read,
        size: Option<u64>,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        unconditional(options)?;
        self.ensure_crypt(ctx, address)?;
        // rclone encrypts here, so RPool never sees the stored bytes.
        self.write_streamed(ctx, address, source, size, options, false)
    }
    /// Only reachable through `RcloneBackend::for_crypt_base`, whose bytes are
    /// already encrypted by RPool. These are the bytes the provider stores,
    /// so their hash is compared with the provider's (see `stored_hash`).
    pub(super) fn write_ungated(
        &self,
        ctx: &OperationContext,
        address: &str,
        source: &mut dyn Read,
        size: Option<u64>,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        self.write_streamed(ctx, address, source, size, options, true)
    }
    /// Shared upload body: `rcat` with single retries, account admission and caps,
    /// parent-folder creation, stall-supervised streaming, traffic/account accounting,
    /// and (when `check_stored_hash`) comparison or deferral of the provider's stored hash.
    fn write_streamed(
        &self,
        ctx: &OperationContext,
        address: &str,
        source: &mut dyn Read,
        size: Option<u64>,
        options: &WriteOptions,
        check_stored_hash: bool,
    ) -> Result<WriteReceipt, StorageError> {
        unconditional(options)?;
        let mut args = vec![
            "rcat".into(),
            "--retries".into(),
            "1".into(),
            "--low-level-retries".into(),
            "1".into(),
        ];
        if let Some(size) = size {
            args.extend(["--size".into(), size.to_string().into()]);
        }
        let account = self.write_lane(ctx, address);
        if account.as_ref().is_some_and(|(_, kind)| kind == "drive") {
            // Drive's daily upload limit then fails at once with a clear text.
            args.push("--drive-stop-on-upload-limit".into());
        }
        args.extend(["--".into(), address.into()]);
        let meter = self.meter_name(address).unwrap_or_default();
        if let Some((name, kind)) = &account {
            crate::storage::account::runtime::admit(&meter, name, kind)?;
        }
        self.ensure_parent_dir(ctx, address);
        // The source stream is consumed, so a rejected upload is reported as
        // retriable (RateLimited) instead of being retried here.
        let _permits = self.mutation_permits(ctx, address)?;
        let op = self.op(address, traffic::Direction::Upload);
        let hash = if check_stored_hash {
            self.stored_hash_kinds(ctx, address)
                .and_then(|kinds| crate::storage::stored_hash::StreamHash::for_provider(&kinds))
        } else {
            None
        };
        let mut hashing = crate::storage::stored_hash::Hashing {
            inner: source,
            hash,
        };
        let mut counted = Counted {
            inner: &mut hashing,
            bytes: 0,
        };
        // A stalled transfer is killed and reported as a retriable timeout.
        let mut command = self.command_for(address, &args, TpsLane::Upload);
        if let Some((base, _)) = &account {
            // One shard is one upload: no chunk fan-out inside rclone, so
            // the account's simultaneous uploads are its requests in flight.
            // Remote-specific variables override rclone.conf; backends
            // without this option ignore it.
            command.env(chunk_concurrency_variable(base), "1");
        }
        // Small objects go through the warm daemon (no process start, no new
        // backend or TLS set-up per shard); Drive keeps the subprocess for
        // `--drive-stop-on-upload-limit`, accounts with a request rate for
        // their per-process `--tpslimit` (`daemon_for`).
        let daemon = size
            .filter(|size| *size <= daemon::DAEMON_UPLOAD_MAX)
            .filter(|_| !account.as_ref().is_some_and(|(_, kind)| kind == "drive"))
            .and_then(|_| self.upload_daemon_for(address));
        let result = match (daemon, size) {
            (Some(daemon), Some(size)) => {
                upload_buffered(&daemon, &mut command, ctx, address, &mut counted, size, &op)
            }
            _ => process::run_upload(&mut command, ctx, &mut counted, Some(&op)),
        };
        if let Some((name, _)) = &account {
            // Bytes that reached rclone count, whatever the outcome.
            crate::storage::account::runtime::record_upload(name, counted.bytes);
            if result.as_ref().is_err_and(process::is_upload_limit) {
                crate::storage::account::runtime::record_provider_limit(&meter, name);
            }
        }
        if let Ok(size) = result {
            op.acked(size);
        }
        op.finish(&result);
        let size = result?;
        let (hash_verified, stored_hash) = match hashing.hash {
            Some(hash) if options.defer_hash_check => (
                false,
                Some(crate::storage::stored_hash::Expected {
                    address: address.to_owned(),
                    kind: hash.kind().to_owned(),
                    size,
                    value: hash.finish(),
                }),
            ),
            Some(hash) => (self.stored_hash_matches(ctx, address, size, hash), None),
            None => (false, None),
        };
        Ok(WriteReceipt {
            size,
            version: None,
            hash_verified,
            stored_hash,
        })
    }
    /// Hash kinds the provider behind `address` reports, cached per remote;
    /// None when it reports none RPool can compute, or when it advertised
    /// one but returned none for an object (e.g. an SFTP server without
    /// `md5sum`), so later uploads do not ask again.
    fn stored_hash_kinds(&self, ctx: &OperationContext, address: &str) -> Option<Vec<String>> {
        let name = remote_name(address).ok()?.to_owned();
        let key = format!("{}\u{0}{name}", self.route_identity());
        if let Some(known) = stored_hash_cache().lock().ok()?.get(&key) {
            return known.clone();
        }
        let kinds = self
            .backend_features(ctx, &format!("{name}:"))
            .ok()
            .map(|f| f.hashes)
            .filter(|kinds| crate::storage::stored_hash::StreamHash::for_provider(kinds).is_some());
        stored_hash_cache().lock().ok()?.insert(key, kinds.clone());
        kinds
    }
    /// Whether the provider reports exactly the hash of the bytes sent. Any
    /// doubt is `false`, and the caller then reads the object back in full.
    fn stored_hash_matches(
        &self,
        ctx: &OperationContext,
        address: &str,
        size: u64,
        hash: crate::storage::stored_hash::StreamHash,
    ) -> bool {
        let kind = hash.kind().to_owned();
        let expected = hash.finish();
        match self.object_hash(ctx, address, std::slice::from_ref(&kind)) {
            Ok(Some((stored, reported))) => {
                let value = reported.rsplit(':').next().unwrap_or_default();
                stored == size && value.eq_ignore_ascii_case(&expected)
            }
            Ok(None) => {
                // Advertised but not returned: stop asking this remote.
                if let (Ok(name), Ok(mut cache)) =
                    (remote_name(address), stored_hash_cache().lock())
                {
                    cache.insert(format!("{}\u{0}{name}", self.route_identity()), None);
                }
                false
            }
            Err(_) => false,
        }
    }
    /// Creates the parent folder of `address` once per process, serialized
    /// per remote, so parallel writes into a new folder do not race to create
    /// it (see `dirs`). Failures are ignored: the write creates it as before.
    pub(super) fn ensure_parent_dir(&self, ctx: &OperationContext, address: &str) {
        let config = match &self.config {
            ConfigSelection::Inherited => self
                .environment
                .iter()
                .find(|(key, _)| key == "RCLONE_CONFIG")
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ConfigSelection::File(path) => path.to_string_lossy().into_owned(),
        };
        let instance = format!("{}\u{0}{config}", self.executable.to_string_lossy());
        dirs::ensure_parent(&instance, address, |parent| {
            self.capture(ctx, &["mkdir", "--", parent]).is_ok()
        });
    }
    /// Deletes one object (`rclone deletefile`), retrying provider rejections with
    /// backoff. Used by `RcloneBackend::delete` and speed-test cleanup.
    pub(crate) fn delete_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<(), StorageError> {
        let args = [
            "deletefile",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            address,
        ]
        .map(OsString::from);
        self.retry_rejected(ctx, address, &args, traffic::Direction::Other)
    }
    /// Removes one EMPTY folder (`rclone rmdir`); a folder that still holds
    /// anything is refused by rclone. Never a purge.
    pub(crate) fn rmdir_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<(), StorageError> {
        let args = [
            "rmdir",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            address,
        ]
        .map(OsString::from);
        self.retry_rejected(ctx, address, &args, traffic::Direction::Other)
    }
    /// General per-remote slot plus the write slot of the storage namespace
    /// behind `address`. Resolution happens before any slot is taken.
    pub(super) fn mutation_permits(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<(Option<limit::Permit>, Option<limit::Permit>), StorageError> {
        let lane = self.write_lane(ctx, address);
        let Some((base, kind)) = &lane else {
            return Ok((permit(ctx, address)?, None));
        };
        // The account's write cap is the only cap on its mutations, so a
        // value above the general per-remote cap really applies.
        let dropbox = kind == "dropbox";
        let cap = crate::storage::account::runtime::settings().upload_cap(base, dropbox);
        Ok((None, Some(limit::acquire_write(base, dropbox, cap, ctx)?)))
    }
    /// One slot for a data read: the account's read cap (its own value,
    /// else the default), or the general cap when the account is unknown.
    pub(super) fn read_permit(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<Option<limit::Permit>, StorageError> {
        let Some((base, _)) = self.write_lane(ctx, address) else {
            return permit(ctx, address);
        };
        let cap = crate::storage::account::runtime::settings().download_cap(&base);
        limit::acquire_read(&base, cap, ctx).map(Some)
    }
    /// (bottom remote of the crypt/alias chain, its backend type), cached per
    /// config selection and remote name. Unresolvable -> the addressed name.
    pub(super) fn write_lane(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Option<(String, String)> {
        type Cache = HashMap<(Option<PathBuf>, Option<OsString>, String), (String, String)>;
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        let name = remote_name(address).ok()?;
        let key = (
            match &self.config {
                ConfigSelection::File(path) => Some(path.clone()),
                ConfigSelection::Inherited => None,
            },
            self.environment
                .iter()
                .find(|(k, _)| k == "RCLONE_CONFIG")
                .map(|(_, v)| v.clone()),
            name.to_owned(),
        );
        let cache = CACHE.get_or_init(Default::default);
        if let Some(lane) = cache.lock().ok()?.get(&key) {
            return Some(lane.clone());
        }
        let Ok(config) = self.config_dump(ctx) else {
            return Some((name.to_owned(), String::new()));
        };
        let lane = write_account(&config, name);
        cache.lock().ok()?.insert(key, lane.clone());
        Some(lane)
    }
    /// One mutation without a source stream, retried with bounded backoff
    /// only while the provider definitely rejected it without performing it.
    pub(super) fn retry_rejected(
        &self,
        ctx: &OperationContext,
        address: &str,
        args: &[OsString],
        direction: traffic::Direction,
    ) -> Result<(), StorageError> {
        let mut attempt = 0;
        let mut op = None;
        let meter = self.meter_name(address).unwrap_or_default();
        let account = match direction {
            traffic::Direction::Upload => self.write_lane(ctx, address),
            _ => None,
        };
        if let Some((name, kind)) = &account {
            crate::storage::account::runtime::admit(&meter, name, kind)?;
        }
        loop {
            let result = {
                let _permits = self.mutation_permits(ctx, address)?;
                let op = op.get_or_insert_with(|| self.op(address, direction));
                process::run_metered(
                    &mut self.command_for(address, args, TpsLane::Upload),
                    ctx,
                    None,
                    &mut io::sink(),
                    true,
                    Some(op),
                )
            };
            if let (Some((name, _)), Err(error)) = (&account, &result) {
                if process::is_upload_limit(error) {
                    crate::storage::account::runtime::record_provider_limit(&meter, name);
                }
            }
            match result {
                Err(error)
                    if error.kind() == crate::storage::error::StorageErrorKind::RateLimited
                        && !process::is_upload_limit(&error)
                        && attempt < REJECTED_RETRIES =>
                {
                    if let Some(op) = &op {
                        op.retry();
                    }
                    backoff(ctx, attempt)?;
                    attempt += 1;
                }
                result => {
                    if let Some(op) = op {
                        op.finish(&result);
                    }
                    return result.map(drop);
                }
            }
        }
    }
}

/// 2s, 4s, 8s (+ up to 25% jitter), cancellable.
pub(super) fn backoff(ctx: &OperationContext, attempt: u32) -> Result<(), StorageError> {
    let base = REJECTED_BACKOFF_MS << attempt;
    let mut jitter = [0u8; 8];
    let _ = getrandom::fill(&mut jitter);
    let wait = base + u64::from_le_bytes(jitter) % (base / 4 + 1);
    let until = Instant::now() + Duration::from_millis(wait);
    while Instant::now() < until {
        process::check(ctx)?;
        std::thread::sleep(process::POLL * 5);
    }
    process::check(ctx)
}

/// One slot of the general per-remote cap for the remote of `address`.
pub(super) fn permit(
    ctx: &OperationContext,
    address: &str,
) -> Result<Option<limit::Permit>, StorageError> {
    match remote_name(address) {
        Ok(name) => limit::acquire(name, ctx).map(Some),
        Err(_) => Ok(None),
    }
}

/// Rejects conditional writes: rclone can only overwrite unconditionally, with no expected version.
pub(super) fn unconditional(options: &WriteOptions) -> Result<(), StorageError> {
    if !options.overwrite || options.expected_version.is_some() {
        return Err(StorageError::unsupported("rclone conditional write"));
    }
    Ok(())
}

/// Route identity + remote name -> stored-hash kinds the provider reports (`None` = do not check).
type HashKindCache = std::collections::HashMap<String, Option<Vec<String>>>;

/// Process-wide cache behind `stored_hash_kinds`.
fn stored_hash_cache() -> &'static std::sync::Mutex<HashKindCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashKindCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// A small upload through the daemon: reads the whole `source` (paced like
/// the subprocess path) and sends it with its size. When the daemon cannot
/// answer definitely, or the source is not `size` bytes long, the same bytes
/// go through `command` (`rcat`), which reports exactly as before.
fn upload_buffered(
    daemon: &daemon::Daemon,
    command: &mut std::process::Command,
    ctx: &OperationContext,
    address: &str,
    source: &mut dyn Read,
    size: u64,
    op: &traffic::Op,
) -> Result<u64, StorageError> {
    let mut body = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut chunk = vec![0u8; process::CHUNK];
    loop {
        process::check(ctx)?;
        let n = match source.read(&mut chunk) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result.map_err(|_| process::io_error())?,
        };
        if n == 0 {
            break;
        }
        op.throttle(ctx, n as u64);
        body.extend_from_slice(&chunk[..n]);
        if body.len() as u64 > size {
            break;
        }
    }
    if body.len() as u64 == size {
        match daemon.upload(ctx, address, &body) {
            Ok(()) => {
                op.sent(size);
                return Ok(size);
            }
            Err(daemon::Failure::Definite(error)) => return Err(error),
            Err(daemon::Failure::Fallback) => {}
        }
    }
    // The bytes were already paced while buffering: send them unmetered.
    let sent = process::run_upload(command, ctx, &mut io::Cursor::new(body), None)?;
    op.sent(sent);
    Ok(sent)
}

/// `RCLONE_CONFIG_<REMOTE>_UPLOAD_CONCURRENCY`: rclone upper-cases the
/// remote name as is (symbols included).
pub(super) fn chunk_concurrency_variable(remote: &str) -> String {
    format!("RCLONE_CONFIG_{}_UPLOAD_CONCURRENCY", remote.to_uppercase())
}

#[cfg(test)]
mod chunk_tests {
    #[test]
    fn chunk_concurrency_variable_names_the_remote() {
        assert_eq!(
            super::chunk_concurrency_variable("filen_1"),
            "RCLONE_CONFIG_FILEN_1_UPLOAD_CONCURRENCY"
        );
        assert_eq!(
            super::chunk_concurrency_variable("my-remote.x"),
            "RCLONE_CONFIG_MY-REMOTE.X_UPLOAD_CONCURRENCY"
        );
    }
}
