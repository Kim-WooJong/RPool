//! Single owning rclone data/admin subprocess adapter. Legacy raw addresses and
//! typed keys share the same primitives, classification and crypt write gate.
#[cfg(all(test, unix))]
#[path = "account_rclone_tests.rs"]
mod account_rclone_tests;
mod bwlimit_schedule;
#[cfg(all(test, unix))]
#[path = "copy_tests.rs"]
mod copy_tests;
mod daemon;
#[cfg(all(test, unix))]
#[path = "daemon_tests.rs"]
mod daemon_tests;
mod dirs;
mod http;
mod limit;
mod pacer;
mod process;
#[cfg(test)]
mod tests;
pub(crate) mod traffic;
#[cfg(all(test, unix))]
#[path = "traffic_rclone_tests.rs"]
mod traffic_rclone_tests;

pub(crate) use daemon::ShutdownGuard as DaemonShutdownGuard;

use crate::storage::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ADMIN_LIMIT: usize = 8 * 1024 * 1024;
const LIST_LIMIT: usize = 1 << 30;
/// Extra attempts for a mutation the provider rejected before performing it.
const REJECTED_RETRIES: u32 = 3;
const REJECTED_BACKOFF_MS: u64 = if cfg!(test) { 20 } else { 2000 };

#[derive(Clone)]
pub(crate) enum ConfigSelection {
    Inherited,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Explicit config selection remains available to opt-in backend callers"
        )
    )]
    File(PathBuf),
}

#[derive(Clone)]
pub(crate) struct RcloneContext {
    executable: PathBuf,
    config: ConfigSelection,
    environment: Vec<(OsString, OsString)>,
    /// Read-only calls may use the shared `rclone rcd` daemon. Off in unit
    /// tests unless a test opts in, so tests never start real daemons.
    daemon_allowed: bool,
    /// (base remote name, crypt remote name): traffic on the base counts for
    /// the crypt remote (native crypt writes address the base).
    traffic_alias: Option<(String, String)>,
}

struct BoundedVec {
    bytes: Vec<u8>,
    limit: usize,
}
#[derive(Debug)]
struct AdminOutputCap;
impl std::fmt::Display for AdminOutputCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rclone admin output cap exceeded")
    }
}
impl std::error::Error for AdminOutputCap {}
struct RangeSink<'a> {
    sink: &'a mut dyn Write,
    remaining: u64,
}
impl Write for RangeSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rclone exceeded requested range",
            ));
        }
        self.sink.write_all(bytes)?;
        self.remaining -= bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}
impl Write for BoundedVec {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, AdminOutputCap));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid(detail: &str) -> StorageError {
    StorageError::invalid_input(detail)
}

impl RcloneContext {
    pub(crate) fn new(executable: PathBuf, config: ConfigSelection) -> Self {
        Self {
            executable,
            config,
            environment: std::env::vars_os().collect(),
            daemon_allowed: !cfg!(test),
            traffic_alias: None,
        }
    }
    /// Counts traffic on remote `base` (a remote name) for `alias`.
    pub(crate) fn attribute_traffic(&mut self, base: &str, alias: &str) {
        self.traffic_alias = Some((base.to_owned(), alias.to_owned()));
    }
    /// The remote name `address`'s traffic is counted for.
    fn meter_name(&self, address: &str) -> Option<String> {
        let name = remote_name(address).ok()?;
        Some(match &self.traffic_alias {
            Some((base, alias)) if base == name => alias.clone(),
            _ => name.to_owned(),
        })
    }
    fn op(&self, address: &str, direction: traffic::Direction) -> traffic::Op {
        let op = traffic::Op::begin(self.meter_name(address).as_deref(), direction);
        let upload = match direction {
            traffic::Direction::Upload => true,
            traffic::Direction::Download => false,
            traffic::Direction::Other => return op,
        };
        let settings = crate::storage::account::runtime::settings();
        let mut keys = vec![pacer::Key::Global { upload }];
        if settings.has_account_overrides() {
            if let Some((name, _)) = self.account_of(address) {
                if settings
                    .store
                    .account(&name)
                    .is_some_and(|l| l.bwlimit.is_some())
                {
                    keys.push(pacer::Key::Account { name, upload });
                }
            }
        }
        op.paced(keys)
    }
    /// (account, backend type) of `address` (bottom of its crypt/alias chain).
    fn account_of(&self, address: &str) -> Option<(String, String)> {
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
        self.write_lane(&ctx, address)
    }
    /// rclone `--tpslimit` of the account behind `address`, when it has one.
    fn tpslimit(&self, address: &str) -> Option<f64> {
        let settings = crate::storage::account::runtime::settings();
        if !settings.has_account_overrides() {
            return None;
        }
        settings.tpslimit(&self.account_of(address)?.0)
    }
    /// The shared read daemon, unless the account behind `address` has a
    /// request-rate limit: rclone applies `--tpslimit` per process, so those
    /// reads use subprocesses carrying the flag.
    fn daemon_for(&self, address: &str) -> Option<std::sync::Arc<daemon::Daemon>> {
        if self.tpslimit(address).is_some() {
            return None;
        }
        daemon::get(self)
    }
    /// Which rclone binary and config resolve addresses (paths only, no
    /// secrets): the same address under another config is another object.
    pub(crate) fn route_identity(&self) -> String {
        let config = match &self.config {
            ConfigSelection::Inherited => self
                .environment
                .iter()
                .find(|(key, _)| key == "RCLONE_CONFIG")
                .map(|(_, value)| PathBuf::from(value)),
            ConfigSelection::File(path) => Some(path.clone()),
        };
        format!("{:?}|{:?}", self.executable, config)
    }
    pub(crate) fn inherited(executable: &str) -> Self {
        Self::new(executable.into(), ConfigSelection::Inherited)
    }
    /// The rclone command with exactly this context's executable, config and
    /// environment (also what the read daemon starts with).
    fn base_command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear().envs(self.environment.iter().cloned());
        if let ConfigSelection::File(path) = &self.config {
            command.arg("--config").arg(path);
        }
        command.args(args);
        command
    }
    /// [`Self::base_command`] plus the global bandwidth timetable, which rclone
    /// switches itself during long transfers (per rclone process; RPool's own
    /// pacer additionally caps the sum of the bytes it pipes).
    fn command(&self, args: &[OsString]) -> Command {
        let mut command = self.base_command(&[]);
        if let Some(timetable) = &crate::storage::account::runtime::settings().store.bandwidth {
            command.arg("--bwlimit").arg(timetable);
        }
        command.args(args);
        command
    }
    /// [`Self::command`] plus per-account flags of the account behind `address`.
    fn command_for(&self, address: &str, args: &[OsString]) -> Command {
        let mut command = self.command(&[]);
        if let Some(tps) = self.tpslimit(address) {
            command.arg("--tpslimit").arg(tps.to_string());
        }
        command.args(args);
        command
    }
    pub(crate) fn capture(
        &self,
        ctx: &OperationContext,
        args: &[&str],
    ) -> Result<Vec<u8>, StorageError> {
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: ADMIN_LIMIT,
        };
        let address = args
            .iter()
            .position(|arg| *arg == "--")
            .and_then(|at| args.get(at + 1));
        let _permit = match address {
            Some(address) => permit(ctx, address)?,
            None => None,
        };
        let op = address.map(|address| self.op(address, traffic::Direction::Other));
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut command = match address {
            Some(address) => self.command_for(address, &args),
            None => self.command(&args),
        };
        let result = process::run_metered(&mut command, ctx, None, &mut sink, false, op.as_ref());
        if let Some(op) = op {
            op.finish(&result);
        }
        result?;
        Ok(sink.bytes)
    }
    /// Recursive `lsjson` of a user tree, which may be far larger than admin
    /// output. Still bounded so a runaway listing cannot exhaust memory.
    pub(crate) fn list_recursive(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<Vec<u8>, StorageError> {
        if let Some(daemon) = self.daemon_for(address) {
            let _permit = permit(ctx, address)?;
            let op = self.op(address, traffic::Direction::Other);
            match daemon.list(ctx, address, LIST_LIMIT) {
                Ok(bytes) => {
                    op.received(bytes.len() as u64);
                    op.finish(&Ok::<(), StorageError>(()));
                    return Ok(bytes);
                }
                Err(daemon::Failure::Definite(error)) => {
                    let result = Err(error);
                    op.finish(&result);
                    return result;
                }
                Err(daemon::Failure::Fallback) => {}
            }
        }
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: LIST_LIMIT,
        };
        let args = ["lsjson", "-R", "--no-mimetype", "--", address];
        let _permit = permit(ctx, address)?;
        let op = self.op(address, traffic::Direction::Other);
        let result = process::run_metered(
            &mut self.command_for(
                address,
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
            ),
            ctx,
            None,
            &mut sink,
            false,
            Some(&op),
        );
        op.finish(&result);
        result?;
        Ok(sink.bytes)
    }
    pub(crate) fn config_dump(&self, ctx: &OperationContext) -> Result<Value, StorageError> {
        let bytes = self.capture(ctx, &["config", "dump"])?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid rclone config JSON"))?;
        if !value.is_object() {
            return Err(invalid("rclone config must be an object"));
        }
        Ok(value)
    }
    pub(crate) fn ensure_crypt(
        &self,
        ctx: &OperationContext,
        destination: &str,
    ) -> Result<(), StorageError> {
        process::check(ctx)?;
        let remote = remote_name(destination)?;
        self.check_policy_environment(false)?;
        let config = self.config_dump(ctx)?;
        let entry = config
            .get(remote)
            .ok_or_else(|| invalid("rclone destination remote is not configured"))?;
        if !entry
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("crypt"))
        {
            return Err(invalid("refusing write to non-crypt destination"));
        }
        match entry.get("no_data_encryption") {
            None | Some(Value::Bool(false)) => Ok(()),
            Some(Value::String(text))
                if matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "no" | "off"
                ) =>
            {
                Ok(())
            }
            Some(Value::Bool(true)) => Err(invalid("crypt data encryption disabled")),
            Some(Value::String(text))
                if matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                ) =>
            {
                Err(invalid("crypt data encryption disabled"))
            }
            _ => Err(invalid("invalid crypt encryption setting")),
        }
    }
    /// Freeze environment for inspection/I/O and fail closed on overrides that
    /// could make effective policy disagree with config dump. Never log values.
    /// Native crypt also refuses every `RCLONE_CRYPT_*` override, because RPool
    /// encrypts from the config dump alone.
    pub(crate) fn check_policy_environment(&self, native_crypt: bool) -> Result<(), StorageError> {
        if self.environment.iter().any(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            key == "RCLONE_CRYPT_NO_DATA_ENCRYPTION"
                || (native_crypt && key.starts_with("RCLONE_CRYPT_"))
                || (key.starts_with("RCLONE_CONFIG_") && key != "RCLONE_CONFIG_PASS")
        }) {
            return Err(invalid(
                "rclone policy-changing environment override is not supported",
            ));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_test_environment(&mut self, key: &str, value: &str) {
        self.environment.retain(|(k, _)| k != key);
        self.environment.push((key.into(), value.into()));
    }
    #[cfg(test)]
    pub(crate) fn allow_daemon_for_test(&mut self) {
        self.daemon_allowed = true;
    }
    /// `lsjson --stat [--hash --hash-type ...]` output for one address,
    /// through the daemon when it gives a definite answer.
    fn stat_json(
        &self,
        ctx: &OperationContext,
        address: &str,
        hashes: &[String],
    ) -> Result<Vec<u8>, StorageError> {
        if let Some(daemon) = self.daemon_for(address) {
            let opt = if hashes.is_empty() {
                json!({})
            } else {
                json!({"showHash": true, "hashTypes": hashes})
            };
            let _permit = permit(ctx, address)?;
            let op = self.op(address, traffic::Direction::Other);
            let answered = match daemon.stat(ctx, address, opt) {
                Ok(item) => {
                    Some(serde_json::to_vec(&item).map_err(|_| invalid("invalid rclone stat JSON")))
                }
                Err(daemon::Failure::Definite(error)) => Some(Err(error)),
                Err(daemon::Failure::Fallback) => None,
            };
            if let Some(result) = answered {
                if let Ok(bytes) = &result {
                    op.received(bytes.len() as u64);
                }
                op.finish(&result);
                return result;
            }
        }
        let mut args = vec!["lsjson", "--stat"];
        if !hashes.is_empty() {
            args.push("--hash");
            for hash in hashes {
                args.extend(["--hash-type", hash.as_str()]);
            }
        }
        args.extend(["--", address]);
        self.capture(ctx, &args)
    }
    pub(crate) fn stat_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<ObjectMetadata, StorageError> {
        let bytes = self.stat_json(ctx, address, &[])?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid rclone stat JSON"))?;
        let is_dir = value
            .get("IsDir")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid("missing stat IsDir"))?;
        let size = if is_dir {
            0
        } else {
            value
                .get("Size")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing stat Size"))?
        };
        // Objects only: a directory's time moves whenever its children change.
        let modified = (!is_dir)
            .then(|| value.get("ModTime").and_then(Value::as_str))
            .flatten()
            .map(str::to_owned);
        Ok(ObjectMetadata {
            size,
            is_dir,
            version: None,
            modified,
        })
    }
    pub(crate) fn read_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        range: Option<&ReadRange>,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        if let Some(range) = range {
            if range.is_empty() {
                if self.stat_raw(ctx, address)?.is_dir {
                    return Err(invalid("cannot read a directory"));
                }
                return Ok(ReadReceipt {
                    bytes_read: 0,
                    version: None,
                });
            }
            // rclone CLI offset/count are signed int64. Reject rather than wrap.
            if range.offset() > i64::MAX as u64 || range.length() > i64::MAX as u64 {
                return Err(invalid("rclone range exceeds signed 64-bit CLI limits"));
            }
        }
        let (offset, count) = range.map_or((0, None), |r| (r.offset(), Some(r.length())));
        let mut bounded = RangeSink {
            sink,
            remaining: count.unwrap_or(u64::MAX),
        };
        let mut written = 0u64;
        let mut op = None;
        if let Some(daemon) = self.daemon_for(address) {
            let outcome = {
                let _permit = permit(ctx, address)?;
                let op = op.insert(self.op(address, traffic::Direction::Download));
                let mut metered = traffic::Metered {
                    sink: &mut bounded,
                    op,
                    ctx,
                };
                daemon.read(ctx, address, offset, count, &mut metered, &mut written)
            };
            match outcome {
                Ok(()) => {
                    let result = Ok(ReadReceipt {
                        bytes_read: written,
                        version: None,
                    });
                    if let Some(op) = op {
                        op.finish(&result);
                    }
                    return result;
                }
                Err(daemon::Failure::Definite(error)) => {
                    let result = Err(error);
                    if let Some(op) = op {
                        op.finish(&result);
                    }
                    return result;
                }
                Err(daemon::Failure::Fallback) => {}
            }
        }
        // Resume after whatever the daemon already delivered.
        let count = count.map(|count| count - written);
        if count == Some(0) {
            let result = Ok(ReadReceipt {
                bytes_read: written,
                version: None,
            });
            if let Some(op) = op {
                op.finish(&result);
            }
            return result;
        }
        let mut args = vec![OsString::from("cat")];
        if range.is_some() || written > 0 {
            args.extend(["--offset".into(), (offset + written).to_string().into()]);
        }
        if let Some(count) = count {
            args.extend(["--count".into(), count.to_string().into()]);
        }
        args.extend(["--".into(), address.into()]);
        let _permit = permit(ctx, address)?;
        let op = op.unwrap_or_else(|| self.op(address, traffic::Direction::Download));
        let result = process::run_metered(
            &mut self.command_for(address, &args),
            ctx,
            None,
            &mut bounded,
            false,
            Some(&op),
        )
        .map(|bytes_read| ReadReceipt {
            bytes_read: written + bytes_read,
            version: None,
        });
        op.finish(&result);
        result
    }
    pub(crate) fn read_all_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let cap = limit.unwrap_or(usize::MAX);
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: cap,
        };
        let range = limit.map(|n| ReadRange::new(0, n as u64)).transpose()?;
        self.read_raw(ctx, address, range.as_ref(), &mut sink)?;
        Ok(sink.bytes)
    }
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
    fn write_ungated(
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
        let result = process::run_metered(
            &mut self.command_for(address, &args),
            ctx,
            Some(&mut counted),
            &mut io::sink(),
            true,
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Native copy implements the retained same-backend copy contract"
        )
    )]
    pub(crate) fn copy_raw(
        &self,
        ctx: &OperationContext,
        source: &str,
        destination: &str,
    ) -> Result<(), StorageError> {
        self.ensure_crypt(ctx, destination)?;
        let args = [
            "copyto",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            source,
            destination,
        ]
        .map(OsString::from);
        self.ensure_parent_dir(ctx, destination);
        self.retry_rejected(ctx, destination, &args, traffic::Direction::Upload)
    }
    /// Copies one object from `source` to `destination` with `rclone copyto`,
    /// never overwriting: an existing destination is refused before the copy,
    /// and `--ignore-existing` closes the race window (a destination that
    /// appears in between is left alone and fails the caller's verification).
    /// The destination must pass the crypt write gate (`ensure_crypt`).
    ///
    /// - Same rclone remote name (e.g. `c1:old/x` -> `c1:new/x`): rclone uses
    ///   the backend's server-side copy when it has one (`Features.Copy`; a
    ///   crypt remote has it when its base has it). A crypt server-side copy
    ///   copies the base object verbatim under the encrypted destination name,
    ///   so no data passes through this PC and the ciphertext stays valid (the
    ///   file nonce lives in the object header; the data key is the remote's).
    ///   Without server-side copy rclone falls back to a streamed copy.
    /// - Different remotes: rclone streams the object through this PC
    ///   (decrypting and re-encrypting for crypt remotes); no temp file.
    pub(crate) fn copy_object(
        &self,
        ctx: &OperationContext,
        source: &str,
        destination: &str,
    ) -> Result<(), StorageError> {
        remote_name(source)?;
        self.ensure_crypt(ctx, destination)?;
        // Bucket-based backends stat a missing key as a directory; copyto
        // refuses to replace a real directory by itself.
        match self.stat_raw(ctx, destination) {
            Ok(metadata) if !metadata.is_dir => {
                return Err(StorageError::AlreadyExists {
                    path: "copy destination".into(),
                })
            }
            Ok(_) => {}
            Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let args = [
            "copyto",
            "--ignore-existing",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            source,
            destination,
        ]
        .map(OsString::from);
        self.ensure_parent_dir(ctx, destination);
        self.retry_rejected(ctx, destination, &args, traffic::Direction::Upload)
    }
    /// `rclone backend features <remote>`: server-side copy support and hashes.
    pub(crate) fn backend_features(
        &self,
        ctx: &OperationContext,
        remote: &str,
    ) -> Result<BackendFeatures, StorageError> {
        let bytes = self.capture(ctx, &["backend", "features", "--", remote])?;
        parse_backend_features(&bytes)
    }
    /// What a copy inside the crypt remote of `address` can do: whether its
    /// backend copies server-side and which hash (if any) the crypt's base
    /// reports for the ciphertext objects. Refuses non-crypt remotes.
    pub(crate) fn crypt_copy_capabilities(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<CryptCopyCapabilities, StorageError> {
        let name = remote_name(address)?;
        let config = self.config_dump(ctx)?;
        let entry = config
            .get(name)
            .ok_or_else(|| invalid("rclone remote is not configured"))?;
        if !entry
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("crypt"))
        {
            return Err(invalid("not a crypt remote"));
        }
        let base = entry
            .get("remote")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| invalid("crypt remote has no base"))?
            .to_owned();
        let own = self.backend_features(ctx, &format!("{name}:"))?;
        let below = self.backend_features(ctx, &base)?;
        Ok(CryptCopyCapabilities {
            server_side_copy: own.copy,
            hashes: preferred_hashes(&below.hashes),
            base,
        })
    }
    /// Address of the ciphertext object behind a crypt `address` on the crypt's
    /// `base` (from `crypt_copy_capabilities`). rclone encrypts the name
    /// (`cryptdecode --reverse`), so every name-encryption option is honoured.
    pub(crate) fn crypt_base_object(
        &self,
        ctx: &OperationContext,
        address: &str,
        base: &str,
    ) -> Result<String, StorageError> {
        let name = remote_name(address)?;
        let path = &address[name.len() + 1..];
        if path.is_empty() {
            return Err(invalid("crypt object path is empty"));
        }
        let remote = format!("{name}:");
        let bytes = self.capture(ctx, &["cryptdecode", "--reverse", "--", &remote, path])?;
        let encrypted = parse_cryptdecode(&bytes, path)?;
        Ok(join_base(base, &encrypted))
    }
    /// (size, `type:value`) of one plain object for the first of `hashes` it
    /// reports, or None when it reports none of them (backends may advertise
    /// a hash they do not return for every object).
    pub(crate) fn object_hash(
        &self,
        ctx: &OperationContext,
        address: &str,
        hashes: &[String],
    ) -> Result<Option<(u64, String)>, StorageError> {
        if hashes.is_empty() {
            return Ok(None);
        }
        let bytes = self.stat_json(ctx, address, hashes)?;
        parse_object_hash(&bytes, hashes)
    }
    /// Creates the parent folder of `address` once per process, serialized
    /// per remote, so parallel writes into a new folder do not race to create
    /// it (see `dirs`). Failures are ignored: the write creates it as before.
    fn ensure_parent_dir(&self, ctx: &OperationContext, address: &str) {
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
    /// Starts the shared read daemon now (when allowed), so a later timed
    /// read does not include the daemon's own start-up.
    pub(crate) fn warm_read_daemon(&self) {
        let _ = daemon::get(self);
    }
    /// General per-remote slot plus the write slot of the storage namespace
    /// behind `address`. Resolution happens before any slot is taken.
    fn mutation_permits(
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
    fn write_lane(&self, ctx: &OperationContext, address: &str) -> Option<(String, String)> {
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
    fn retry_rejected(
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
fn backoff(ctx: &OperationContext, attempt: u32) -> Result<(), StorageError> {
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

/// The storage namespace a write to remote `name` lands in: follows
/// single-remote wrappers (`crypt`, `alias`, ...) through their `remote =`.
pub(crate) fn write_base(config: &Value, name: &str) -> (String, bool) {
    let (base, kind) = write_account(config, name);
    (base, kind == "dropbox")
}

/// [`write_base`] with the bottom remote's backend type (lowercase; empty
/// when unknown): the cloud account a write to `name` is charged to.
pub(crate) fn write_account(config: &Value, name: &str) -> (String, String) {
    let mut current = name.to_owned();
    for _ in 0..8 {
        let Some(entry) = config.get(&current) else {
            return (current, String::new());
        };
        let kind = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            kind.as_str(),
            "crypt" | "alias" | "chunker" | "compress" | "hasher" | "cache"
        ) {
            return (current, kind);
        }
        let Some(next) = entry
            .get("remote")
            .and_then(Value::as_str)
            .and_then(|remote| remote_name(remote).ok())
        else {
            return (current, kind);
        };
        current = next.to_owned();
    }
    (current, String::new())
}

/// Counts the bytes rclone read from an upload source.
struct Counted<'a> {
    inner: &'a mut dyn Read,
    bytes: u64,
}
impl Read for Counted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

/// One slot of the general per-remote cap for the remote of `address`.
fn permit(ctx: &OperationContext, address: &str) -> Result<Option<limit::Permit>, StorageError> {
    match remote_name(address) {
        Ok(name) => limit::acquire(name, ctx).map(Some),
        Err(_) => Ok(None),
    }
}

/// Parsed `rclone backend features` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BackendFeatures {
    /// `Features.Copy`: the backend copies objects server-side.
    pub copy: bool,
    /// Supported hash types, rclone names (`md5`, `sha1`, ...).
    pub hashes: Vec<String>,
}

/// Copy abilities inside one crypt remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CryptCopyCapabilities {
    pub server_side_copy: bool,
    /// Hashes of the base's ciphertext objects, preferred first, used to
    /// verify a server-side copy. Empty when the base reports none.
    pub hashes: Vec<String>,
    /// The crypt's base (`remote =` in its config section).
    pub base: String,
}

pub(crate) fn parse_backend_features(bytes: &[u8]) -> Result<BackendFeatures, StorageError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid rclone features JSON"))?;
    let copy = value
        .get("Features")
        .and_then(|f| f.get("Copy"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let hashes = value
        .get("Hashes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|h| !h.is_empty() && *h != "none")
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(BackendFeatures { copy, hashes })
}

/// Up to three advertised hashes: strong, widely supported ones first, any
/// other advertised hash when none of those is.
pub(crate) fn preferred_hashes(hashes: &[String]) -> Vec<String> {
    const ORDER: &[&str] = &["sha256", "sha1", "md5", "blake3", "sha512", "xxh128"];
    let mut out: Vec<String> = ORDER
        .iter()
        .filter(|want| hashes.iter().any(|h| h == *want))
        .take(3)
        .map(|h| (*h).to_owned())
        .collect();
    if out.is_empty() {
        out.extend(hashes.first().cloned());
    }
    out
}

/// `cryptdecode --reverse` prints `<input> \t <encrypted>` per argument.
pub(crate) fn parse_cryptdecode(bytes: &[u8], path: &str) -> Result<String, StorageError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid cryptdecode output"))?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let line = lines
        .next()
        .ok_or_else(|| invalid("empty cryptdecode output"))?;
    if lines.next().is_some() {
        return Err(invalid("unexpected cryptdecode output"));
    }
    let (input, encrypted) = line
        .split_once('\t')
        .ok_or_else(|| invalid("unexpected cryptdecode output"))?;
    let encrypted = encrypted.trim();
    if input.trim() != path
        || encrypted.is_empty()
        || encrypted.starts_with('/')
        || encrypted.contains(':')
        || encrypted.split('/').any(|s| matches!(s, "" | "." | ".."))
    {
        return Err(invalid("unexpected cryptdecode output"));
    }
    Ok(encrypted.to_owned())
}

pub(crate) fn join_base(base: &str, relative: &str) -> String {
    if base.ends_with([':', '/']) {
        format!("{base}{relative}")
    } else {
        format!("{base}/{relative}")
    }
}

pub(crate) fn parse_object_hash(
    bytes: &[u8],
    hashes: &[String],
) -> Result<Option<(u64, String)>, StorageError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid rclone stat JSON"))?;
    if value.get("IsDir").and_then(Value::as_bool) != Some(false) {
        return Err(invalid("expected object, found directory"));
    }
    let size = value
        .get("Size")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing stat Size"))?;
    let reported = value.get("Hashes");
    Ok(hashes.iter().find_map(|hash| {
        reported
            .and_then(|h| h.get(hash))
            .and_then(Value::as_str)
            .filter(|h| !h.is_empty())
            .map(|h| (size, format!("{hash}:{}", h.to_ascii_lowercase())))
    }))
}

fn unconditional(options: &WriteOptions) -> Result<(), StorageError> {
    if !options.overwrite || options.expected_version.is_some() {
        return Err(StorageError::unsupported("rclone conditional write"));
    }
    Ok(())
}

pub(crate) fn remote_name(address: &str) -> Result<&str, StorageError> {
    let (name, _) = address
        .split_once(':')
        .ok_or_else(|| invalid("expected configured rclone remote"))?;
    if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(invalid("invalid configured remote"));
    }
    if name.len() == 1
        && name.as_bytes()[0].is_ascii_alphabetic()
        && address
            .as_bytes()
            .get(2)
            .is_some_and(|c| matches!(c, b'/' | b'\\'))
    {
        return Err(invalid("Windows drive is not a crypt remote"));
    }
    Ok(name)
}

pub(crate) struct RcloneBackend {
    id: BackendId,
    context: RcloneContext,
    root: String,
    legacy_object: Option<String>,
    /// Base remote of a native crypt backend: writes carry RPool ciphertext.
    crypt_base: bool,
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
    fn address(&self, key: &ObjectKey) -> Result<String, StorageError> {
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
