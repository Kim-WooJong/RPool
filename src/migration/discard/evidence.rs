//! Optional pre-write ownership receipts. Missing old receipts mean preserve;
//! a random id or Verified alone is NOT evidence that objects were absent.
use super::*;
use crate::config_sync::encoding_probe::Probe;
use crate::migration::journal::Journal;
use crate::migration::model::{Entry, RecordState};

/// Immutable side document emitted BEFORE the first output write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Creation {
    pub version: u32,
    pub migration_id: String,
    pub pool: String,
    pub entry: String,
    pub archive_id: String,
    pub attempt_id: String,
    pub claim_unix: u64,
    pub observed_unix: u64,
    pub plan_fingerprint: String,
    pub source: Manifest,
    pub roots: Vec<String>,
    pub bindings: BTreeMap<String, String>,
}

/// Exact output id, never an arbitrary prefix.
pub(super) fn name(id: &str) -> String { format!("copy-created-{id}.json") }

/// Reject any second filename view, including metadata-only alternate views.
pub(super) fn single_view(probe: &Probe) -> Result<()> {
    let current = probe.current_encoding.as_str();
    if !matches!(current, "base32" | "base32768") { bail!("unsupported filename encoding"); }
    let alternate = if current == "base32" { "base32768" } else { "base32" };
    if !probe.list(alternate, "", 8 * 1024 * 1024)?.is_empty() {
        bail!("mixed filename encodings: reconcile explicit namespace views before deletion; no automatic merge");
    }
    probe.ensure_unchanged()
}

/// Receipt failures preserve copies, without disabling existing migration runs.
pub(crate) fn record_creation(rclone: &str, journal: &Journal, entry: &Entry,
    source: &Manifest, target: &PoolDefinition, id: &str) {
    if let Err(error) = capture(rclone, journal, entry, source, target, id) {
        eprintln!("[migration] copy ownership receipt unavailable; future abandoned cleanup preserves {id}: {error:#}");
    }
}

/// All destination roots must be observed free of this archive BEFORE writing.
fn capture(rclone: &str, journal: &Journal, entry: &Entry, source: &Manifest,
    target: &PoolDefinition, id: &str) -> Result<()> {
    if !safe_id(id) || id == source.archive_id { bail!("unsafe replacement identity"); }
    let plan = journal.load_plan()?.context("missing migration plan")?;
    let mut claims = BTreeMap::new();
    for r in journal.records()? {
        if r.state == RecordState::Claimed && r.entry == entry.archive_id && r.new_archive_id.as_deref() == Some(id) {
            claims.insert(digest(&r)?, r);
        }
    }
    if claims.len() != 1 { bail!("no unique immutable creation claim"); }
    let roots = crate::remote_root::apply_remote_roots(target.remotes.clone())?;
    if roots.is_empty() || roots.len() > MAX_NAMESPACES { bail!("destination scope exceeds bound"); }
    let mut bindings = BTreeMap::new();
    for root in &roots {
        let (remote, path) = split(root)?;
        let probe = Probe::new(Path::new(rclone), remote)?;
        single_view(&probe)?;
        let prefix = if path.is_empty() { format!("{id}/") } else { format!("{path}/{id}/") };
        if probe.cleanup_files(&probe.current_encoding)?.iter().any(|e|
            e.path.starts_with(&prefix) || e.path == prefix.trim_end_matches('/')) {
            bail!("replacement namespace existed before writing; no ownership asserted");
        }
        bindings.insert(remote.into(), format!("{}:{}", probe.current_encoding, probe.binding()));
        probe.ensure_unchanged()?;
    }
    let claim = claims.values().next().context("claim missing")?;
    let proof = Creation { version: 1, migration_id: plan.migration_id.clone(), pool: plan.pool.clone(),
        entry: entry.archive_id.clone(), archive_id: id.into(), attempt_id: claim.attempt_id.clone(),
        claim_unix: claim.ts_unix, observed_unix: crate::utils::now_unix(),
        plan_fingerprint: digest(&plan)?, source: source.clone(), roots, bindings };
    if journal.create_document(&name(id), &serde_json::to_vec(&proof)?)?.is_some() {
        bail!("different creation receipt already exists; preserved");
    }
    Ok(())
}
