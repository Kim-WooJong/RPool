//! Single owning rclone data/admin subprocess adapter. Legacy raw addresses and
//! typed keys share the same primitives, classification and crypt write gate.
mod process;
#[cfg(test)]
mod tests;

use crate::storage::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;
use serde_json::Value;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;

const ADMIN_LIMIT: usize = 8 * 1024 * 1024;

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
        }
    }
    pub(crate) fn inherited(executable: &str) -> Self {
        Self::new(executable.into(), ConfigSelection::Inherited)
    }
    fn command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear().envs(self.environment.iter().cloned());
        if let ConfigSelection::File(path) = &self.config {
            command.arg("--config").arg(path);
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
        process::run(
            &mut self.command(&args.iter().map(OsString::from).collect::<Vec<_>>()),
            ctx,
            None,
            &mut sink,
            false,
        )?;
        Ok(sink.bytes)
    }
    /// Recursive `lsjson` of a user tree, which may be far larger than admin
    /// output. Still bounded so a runaway listing cannot exhaust memory.
    pub(crate) fn list_recursive(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<Vec<u8>, StorageError> {
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: 1 << 30,
        };
        let args = ["lsjson", "-R", "--no-mimetype", "--", address];
        process::run(
            &mut self.command(&args.iter().map(OsString::from).collect::<Vec<_>>()),
            ctx,
            None,
            &mut sink,
            false,
        )?;
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
    pub(crate) fn stat_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<ObjectMetadata, StorageError> {
        let bytes = self.capture(ctx, &["lsjson", "--stat", "--", address])?;
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
        Ok(ObjectMetadata {
            size,
            is_dir,
            version: None,
        })
    }
    pub(crate) fn read_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        range: Option<&ReadRange>,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let mut args = vec![OsString::from("cat")];
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
            args.extend([
                "--offset".into(),
                range.offset().to_string().into(),
                "--count".into(),
                range.length().to_string().into(),
            ]);
        }
        args.extend(["--".into(), address.into()]);
        let mut bounded = RangeSink {
            sink,
            remaining: range.map(ReadRange::length).unwrap_or(u64::MAX),
        };
        let bytes_read = process::run(&mut self.command(&args), ctx, None, &mut bounded, false)?;
        Ok(ReadReceipt {
            bytes_read,
            version: None,
        })
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
        args.extend(["--".into(), address.into()]);
        let size = process::run(
            &mut self.command(&args),
            ctx,
            Some(source),
            &mut io::sink(),
            true,
        )?;
        Ok(WriteReceipt {
            size,
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
        process::run(&mut self.command(&args), ctx, None, &mut io::sink(), true)?;
        Ok(())
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
        process::run(&mut self.command(&args), ctx, None, &mut io::sink(), true)?;
        Ok(())
    }
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
