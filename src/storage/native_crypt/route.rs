//! Routes `crypt-remote:path` addresses to a native `CryptBackend` over the crypt
//! remote's base, for pools that opt in with `native_crypt`.
//!
//! This is also the native write gate. A write is allowed only when all of these hold:
//! - the address names a configured crypt remote that `CryptConfig` fully supports,
//!   which refuses `no_data_encryption`, `pass_bad_blocks` and unknown options;
//! - its base is a plain configured remote, not crypt or another wrapper that
//!   could hide one;
//! - no `RCLONE_CRYPT_*` or `RCLONE_CONFIG_*` environment override is present.
//!
//! The config is read once per router, so one command uses one fixed key set.
//! Failures are not cached, so a transient `config dump` error is retried.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use super::CryptBackend;
use crate::crypt::cipher::Cipher;
use crate::crypt::options::CryptConfig;
use crate::storage::error::StorageError;
use crate::storage::rclone::{remote_name, RcloneBackend, RcloneContext};
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::{OperationContext, StorageBackend};

/// Capability to write unencrypted bytes to a crypt base. Only this module can
/// create one.
pub(crate) struct BaseAccess(());

/// Remote types that may wrap another remote (and so a crypt) or rewrite names.
const WRAPPING_TYPES: &[&str] = &[
    "crypt", "alias", "union", "combine", "chunker", "hasher", "compress", "cache",
];

/// A backend and the plaintext key to write through it.
pub(crate) type Route = (Arc<dyn StorageBackend>, ObjectKey);

/// Router from `crypt-remote:path` addresses to cached `CryptBackend`s.
/// Created by `storage::writer` and `speedtest::engine` when native crypt is enabled.
pub(crate) struct NativeCrypt {
    /// rclone context used for `config dump` and for the base backends.
    context: RcloneContext,
    /// `rclone config dump` read once per router; only successes are stored.
    dump: OnceLock<Value>,
    /// Built crypt backends keyed by crypt remote alias.
    backends: Mutex<BTreeMap<String, Arc<CryptBackend>>>,
}

/// Shorthand for `StorageError::invalid_input`.
fn invalid(detail: &str) -> StorageError {
    StorageError::invalid_input(detail)
}

/// A dump section as strings. Values are never copied into error text.
fn section(entry: &Value) -> Result<BTreeMap<String, String>, StorageError> {
    let object = entry
        .as_object()
        .ok_or_else(|| invalid("rclone remote entry is not an object"))?;
    object
        .iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Bool(flag) => flag.to_string(),
                _ => return Err(invalid("unsupported rclone option value type")),
            };
            Ok((key.clone(), text))
        })
        .collect()
}

/// Rejects a crypt base that is on-the-fly (`:type:`), unconfigured, or a
/// wrapping remote type that could hide another crypt.
fn validate_base(dump: &Value, base: &str) -> Result<(), StorageError> {
    if base.starts_with(':') {
        return Err(invalid("native crypt base must be a configured remote"));
    }
    let alias =
        remote_name(base).map_err(|_| invalid("native crypt base must be a configured remote"))?;
    let kind = dump
        .get(alias)
        .and_then(|entry| entry.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("native crypt base remote is not configured"))?;
    if WRAPPING_TYPES.iter().any(|t| kind.eq_ignore_ascii_case(t)) {
        return Err(invalid(
            "native crypt base must not be a crypt or wrapping remote",
        ));
    }
    Ok(())
}

impl NativeCrypt {
    /// Router with no dump loaded and no backends built yet.
    pub(crate) fn new(context: RcloneContext) -> Self {
        Self {
            context,
            dump: OnceLock::new(),
            backends: Mutex::new(BTreeMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_dump(context: RcloneContext, dump: Value) -> Self {
        let router = Self::new(context);
        let _ = router.dump.set(dump);
        router
    }

    /// Config dump, loaded on first use after refusing rclone env overrides.
    fn dump(&self, ctx: &OperationContext) -> Result<&Value, StorageError> {
        self.context.check_policy_environment(true)?;
        if let Some(dump) = self.dump.get() {
            return Ok(dump);
        }
        let dump = self.context.config_dump(ctx)?;
        Ok(self.dump.get_or_init(|| dump))
    }

    /// Builds the `CryptBackend` for crypt remote `alias`: validates the crypt
    /// options and base, derives the cipher and attributes base traffic to `alias`.
    fn build(&self, dump: &Value, alias: &str) -> Result<Arc<CryptBackend>, StorageError> {
        let entry = dump
            .get(alias)
            .ok_or_else(|| invalid("rclone destination remote is not configured"))?;
        let section = section(entry)?;
        if !section
            .get("type")
            .is_some_and(|t| t.eq_ignore_ascii_case("crypt"))
        {
            return Err(invalid("refusing write to non-crypt destination"));
        }
        let config = CryptConfig::from_section(&section).map_err(|error| {
            invalid(&format!(
                "unsupported crypt remote for native crypt: {error}"
            ))
        })?;
        validate_base(dump, &config.remote)?;
        let cipher = Cipher::new(&config).map_err(|_| invalid("crypt key derivation failed"))?;
        // Base traffic is this crypt remote's traffic in the monitor.
        let mut context = self.context.clone();
        context.attribute_traffic(remote_name(&config.remote)?, alias);
        let base = RcloneBackend::for_crypt_base(
            BackendId::new(format!("native-crypt-base-{alias}"))?,
            context,
            config.remote.clone(),
            BaseAccess(()),
        )?;
        Ok(Arc::new(CryptBackend::new(
            BackendId::new(format!("native-crypt-{alias}"))?,
            Arc::new(base),
            Arc::new(cipher),
        )))
    }

    /// Cached or newly built backend for the remote named in `raw`.
    fn backend(
        &self,
        ctx: &OperationContext,
        raw: &str,
    ) -> Result<Arc<CryptBackend>, StorageError> {
        let alias = remote_name(raw)?;
        let dump = self.dump(ctx)?;
        let mut backends = self.backends.lock().map_err(|_| StorageError::Other {
            detail: "native crypt route lock poisoned".into(),
        })?;
        if let Some(backend) = backends.get(alias) {
            return Ok(backend.clone());
        }
        let backend = self.build(dump, alias)?;
        backends.insert(alias.to_string(), backend.clone());
        Ok(backend)
    }

    /// The native write gate for a destination (a root or an object address).
    pub(crate) fn ensure(&self, ctx: &OperationContext, raw: &str) -> Result<(), StorageError> {
        self.backend(ctx, raw).map(|_| ())
    }

    /// Backend and plaintext key for an object address. `None` means the path is
    /// not a portable, canonical object key (for example non-ASCII, `a//b`, `./a`
    /// or `a/`), which rclone would clean or refuse; the caller then uses the
    /// gated rclone crypt route, which stores the same bytes.
    pub(crate) fn route(
        &self,
        ctx: &OperationContext,
        raw: &str,
    ) -> Result<Option<Route>, StorageError> {
        let backend = self.backend(ctx, raw)?;
        let (_, path) = raw.split_once(':').expect("remote_name checked the colon");
        if path
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
        {
            return Ok(None);
        }
        match ObjectKey::new(path) {
            Ok(key) => Ok(Some((backend, key))),
            Err(_) => Ok(None),
        }
    }
}
