//! Archive id generation and `remote:path` string helpers.
use crate::prelude::*;

/// Archive id for a source file: its sanitized name plus 16 hex chars of BLAKE3
/// over name, size and modification time. Used by `commands::put`.
pub(crate) fn make_archive_id(path: &Path, meta: &fs::Metadata) -> String {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seed = format!("{filename}\0{}\0{modified}", meta.len());
    let digest = blake3::hash(seed.as_bytes()).to_hex().to_string();
    format!("{}-{}", safe_component(&filename), &digest[..16])
}

/// Up to 48 chars of `input` with anything but ASCII alphanumerics and `.-_`
/// replaced by `_`; `archive` when empty.
pub(crate) fn safe_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(64));
    for c in input.chars().take(48) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "archive".to_string()
    } else {
        out
    }
}

/// Joins `relative` onto a remote base, without a double `/` and without a `/`
/// right after `remote:`.
pub(crate) fn remote_join(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with(':') {
        format!("{base}{relative}")
    } else {
        format!("{base}/{relative}")
    }
}

/// The path of `object` relative to the remote `base` (empty when equal); errors
/// when the object is not under the base. Used by migration and drive history cleanup.
pub(crate) fn relative_remote_object(base: &str, object: &str) -> Result<String> {
    let base = base.trim_end_matches('/');
    if base.ends_with(':') {
        if let Some(rest) = object.strip_prefix(base) {
            return Ok(rest.trim_start_matches('/').to_string());
        }
    } else {
        let prefix = format!("{base}/");
        if let Some(rest) = object.strip_prefix(&prefix) {
            return Ok(rest.to_string());
        }
        if object == base {
            return Ok(String::new());
        }
    }
    bail!("object is not under its declared remote base: base={base} object={object}")
}

/// `path` with `suffix` appended to its final component text (e.g. `.tmp`, `.bak`).
pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
