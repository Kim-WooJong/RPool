//! Separate fail-closed cleanup of NEW copies of one abandoned migration.
//! Never weakens retire, changes abandonment/adoption, or removes directories.
mod evidence;
mod observe;
mod perform;
use crate::prelude::*;
pub(crate) use evidence::record_creation;
pub(crate) use observe::preview;
pub(crate) use perform::run;

/// Fixed bounds are refusals, not partial-success truncations.
pub(super) const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_DOCS: usize = 10_000;
pub(super) const MAX_NAMESPACES: usize = 16;
pub(super) const PREVIEW_SECONDS: u64 = 15 * 60;
pub(super) const GRACE_SECONDS: u64 = 7 * 86_400;

/// Read-only preview inputs. All known workspaces must be supplied explicitly.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    pub workspaces: Vec<PathBuf>,
    pub confirm: bool,
    pub all_pcs_stopped: bool,
    pub all_local_workspaces_listed: bool,
    pub preview_token: Option<String>,
    pub force: bool,
}

/// Exact allowlist object, including its freshly observed plaintext hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Object {
    pub address: String,
    pub root: String,
    pub size: u64,
    pub blake3: String,
    pub archive_id: String,
}

/// Tracked output, retained with reasons or eligible exact objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CopyView {
    pub archive_id: String,
    pub entry: String,
    pub objects: Vec<Object>,
    pub keep_reasons: Vec<String>,
}

/// Bound report: no cloud/cache/lease/history writes are made by preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Report {
    pub version: u32,
    pub pool: String,
    pub migration_id: String,
    pub observed_unix: u64,
    pub plan_fingerprint: String,
    pub namespace_binding: BTreeMap<String, String>,
    pub workspaces: Vec<PathBuf>,
    pub snapshot_fingerprint: String,
    pub reference_fingerprint: String,
    pub preview_token: String,
    pub bounded_completion: bool,
    pub scope: String,
    pub uncertainties: Vec<String>,
    pub copies: Vec<CopyView>,
    pub candidates: Vec<Object>,
    pub candidate_count: u64,
    pub candidate_bytes: u64,
    pub pool_count: u64,
    pub pool_bytes: u64,
    pub guard_refusal: Option<String>,
    pub deletion_ready: bool,
    pub outcomes: BTreeMap<String, String>,
}

/// Deterministic canonical JSON hash, never an inferred ownership proof.
pub(super) fn digest(value: &impl Serialize) -> Result<String> {
    let canonical = serde_json::to_value(value)?;
    Ok(blake3::hash(&serde_json::to_vec(&canonical)?).to_hex().to_string())
}

/// A safe relative object path (not an arbitrary prefix).
pub(super) fn safe_path(path: &str) -> bool {
    !path.is_empty() && !path.contains([':', '\\']) && !path.chars().any(char::is_control)
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// The only accepted output ids are exact journal ids, with safe components.
pub(super) fn safe_id(id: &str) -> bool {
    super::journal::validate_migration_id(id).is_ok() && !id.contains('.')
}

/// Split an exact remote address without permitting traversal or extra remotes.
pub(super) fn split(address: &str) -> Result<(&str, &str)> {
    let (name, path) = address.split_once(':').context("not a remote object address")?;
    if name.is_empty() || name.starts_with('-') || (!path.is_empty() && !safe_path(path.trim_matches('/'))) {
        bail!("unsafe cleanup address");
    }
    Ok((name, path.trim_matches('/')))
}
