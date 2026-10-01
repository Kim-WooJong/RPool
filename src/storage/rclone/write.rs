//! Gated object writes, deletes and mutation retry/permit handling.

use super::*;

impl RcloneContext {
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
        self.write_ungated(ctx, address, source, size, options)
    }
    /// Only reachable through `RcloneBackend::for_crypt_base`, whose bytes are
    /// already encrypted by RPool.
    pub(super) fn write_ungated(
        &self,
        ctx: &OperationContext,
        address: &str,
        source: &mut dyn Read,
        size: Option<u64>,
        options: &WriteOptions,
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
        let mut counted = Counted {
            inner: source,
            bytes: 0,
        };
        // A stalled transfer is killed and reported as a retriable timeout.
        let result = process::run_upload(
            &mut self.command_for(address, &args),
            ctx,
            &mut counted,
            Some(&op),
        );
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
        Ok(WriteReceipt {
            size: result?,
            version: None,
        })
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
        let general = permit(ctx, address)?;
        let write = match &lane {
            Some((base, kind)) => Some(limit::acquire_write(base, kind == "dropbox", ctx)?),
            None => None,
        };
        Ok((general, write))
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
                    &mut self.command(args),
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

pub(super) fn unconditional(options: &WriteOptions) -> Result<(), StorageError> {
    if !options.overwrite || options.expected_version.is_some() {
        return Err(StorageError::unsupported("rclone conditional write"));
    }
    Ok(())
}
