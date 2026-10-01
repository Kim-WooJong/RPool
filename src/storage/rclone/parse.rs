//! Parsing of rclone config/feature/hash output and remote address helpers.

use super::*;

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
