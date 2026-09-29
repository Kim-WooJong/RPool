//! Explicit-source, copy-only reprocessing. Original archives are never mutated.
use crate::manifest::{load_manifest, manifest_fingerprint, validate_manifest};
use crate::prelude::*;
use crate::storage::writer::StorageWriter;
use crate::utils::{append_suffix, read_json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReprocessEntry {
    pub(crate) source: String,
    pub(crate) manifest: Manifest,
    pub(crate) fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReprocessPlan {
    pub(crate) version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_path: PathBuf,
    pub(crate) target: PoolDefinition,
    pub(crate) entries: Vec<ReprocessEntry>,
    pub(crate) input_bytes: u64,
    pub(crate) new_storage_bytes: u64,
    pub(crate) download_bytes: u64,
    pub(crate) upload_bytes: u64,
    pub(crate) estimated_seconds: Option<f64>,
    pub(crate) estimate_note: String,
    #[serde(default)]
    pub(crate) change_summary: Vec<String>,
}

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random identifier: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn private_dir(path: &Path) -> Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

// Create-only, fsynced receipts avoid losing discovery information on interrupted writes.
fn save_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("receipt parent missing")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn storage_bytes(size: u64, target: &PoolDefinition) -> Result<u64> {
    let shard = target.shard_bytes()?.get();
    if target.parity_shards == 0 {
        return Ok(size);
    }
    if size == 0 {
        bail!("empty files cannot be reprocessed into Reed-Solomon archives");
    }
    let data = size / shard + u64::from(size % shard != 0);
    let k = target.data_shards as u64;
    let groups = data / k + u64::from(data % k != 0);
    // Every parity shard is a full shard, even for the final partial group.
    groups
        .checked_mul(target.parity_shards as u64)
        .and_then(|v| v.checked_mul(shard))
        .and_then(|v| size.checked_add(v))
        .context("storage estimate overflow")
}

pub(crate) fn build_plan(
    rclone: &str,
    sources: Vec<String>,
    mut target: PoolDefinition,
    download_mib_s: Option<f64>,
    upload_mib_s: Option<f64>,
) -> Result<ReprocessPlan> {
    target.remotes = crate::remote_root::apply_remote_roots(target.remotes)?;
    super::validate_pool(&target)?;
    for rate in [download_mib_s, upload_mib_s].into_iter().flatten() {
        if !rate.is_finite() || rate <= 0.0 {
            bail!("bandwidth must be a finite positive aggregate MiB/s rate");
        }
    }
    if sources.is_empty() {
        bail!("select explicit manifest sources; pool membership cannot be inferred");
    }
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    let mut input_bytes = 0u64;
    let mut new_storage_bytes = 0u64;
    for source in sources {
        let manifest = load_manifest(rclone, &source)?;
        validate_manifest(&manifest)?;
        if !ids.insert(manifest.archive_id.clone()) {
            bail!("duplicate selected archive: {}", manifest.archive_id);
        }
        input_bytes = input_bytes
            .checked_add(manifest.original_size)
            .context("input size overflow")?;
        new_storage_bytes = new_storage_bytes
            .checked_add(storage_bytes(manifest.original_size, &target)?)
            .context("storage estimate overflow")?;
        entries.push(ReprocessEntry {
            source,
            fingerprint: manifest_fingerprint(&manifest)?,
            manifest,
        });
    }
    // Writer reads every new shard back; final independent full verification rereads again.
    let download_bytes = new_storage_bytes
        .checked_mul(2)
        .and_then(|v| input_bytes.checked_add(v))
        .context("transfer estimate overflow")?;
    let upload_bytes = new_storage_bytes;
    let estimated_seconds = download_mib_s.zip(upload_mib_s).map(|(down, up)| {
        download_bytes as f64 / (down * 1048576.0) + upload_bytes as f64 / (up * 1048576.0)
    });
    if estimated_seconds.is_some_and(|seconds| !seconds.is_finite()) {
        bail!("estimated duration overflow; use realistic positive transfer rates");
    }
    let root = crate::config::app_config_dir()?.join("reprocess");
    fs::create_dir_all(&root)?;
    let operation_id = random_id()?;
    let dir = root.join(&operation_id);
    private_dir(&dir)?;
    let change_summary = describe_changes(&entries, &target);
    let plan = ReprocessPlan {
        version: 2, operation_id, plan_path: dir.join("plan.json"), target, entries,
        change_summary,
        input_bytes, new_storage_bytes, download_bytes, upload_bytes, estimated_seconds,
        estimate_note: "Approximate sequential transfer time using user-supplied aggregate rates. Includes upload readback and final full verification; excludes metadata, encryption overhead, disk/CPU time, retries and degraded-source recovery. Originals remain stored; target capacity is additional. Unknown without both rates. This is initial execution time, not remaining time: resume skips copying completed items but fully reads them again to verify readiness.".into(),
    };
    save_new(&plan.plan_path, &plan)?;
    save_new(
        &dir.join("plan.fingerprint.json"),
        &blake3::hash(&fs::read(&plan.plan_path)?)
            .to_hex()
            .to_string(),
    )?;
    Ok(plan)
}

pub(crate) fn load_plan(plan_path: &Path) -> Result<ReprocessPlan> {
    let plan_path = plan_path.canonicalize()?;
    let dir = plan_path.parent().context("plan directory missing")?;
    let bytes = fs::read(&plan_path)?;
    let expected: String = read_json(&dir.join("plan.fingerprint.json"))?;
    if blake3::hash(&bytes).to_hex().as_str() != expected {
        bail!("saved plan changed; create a new plan");
    }
    let plan: ReprocessPlan = serde_json::from_slice(&bytes)?;
    if plan.version != 1 && plan.version != 2 {
        bail!("unsupported reprocess plan version");
    }
    if plan.plan_path.canonicalize()? != plan_path {
        bail!("invalid saved plan location/version");
    }
    super::validate_pool(&plan.target)?;
    Ok(plan)
}

pub(crate) fn execute_plan(rclone: &str, plan_path: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    let plan_path = plan_path.canonicalize()?;
    let dir = plan_path.parent().context("plan directory missing")?;
    let _execution_lock = lock_plan(dir)?;
    let plan = load_plan(&plan_path)?;
    super::validate_pool(&plan.target)?;
    // Check every source before any remote mutation; restore from the frozen snapshot below.
    for (index, entry) in plan.entries.iter().enumerate() {
        validate_manifest(&entry.manifest)?;
        if manifest_fingerprint(&entry.manifest)? != entry.fingerprint {
            bail!("frozen source manifest fingerprint mismatch");
        }
        if dir.join(format!("completed-{index:08}.json")).exists() {
            continue;
        }
        let current = load_manifest(rclone, &entry.source)?;
        validate_manifest(&current)?;
        if manifest_fingerprint(&current)? != entry.fingerprint
            || manifest_fingerprint(&entry.manifest)? != entry.fingerprint
        {
            bail!(
                "source manifest changed: {}; create a new plan",
                entry.source
            );
        }
    }
    let writer = StorageWriter::for_pool(rclone, plan.target.native_crypt);
    for remote in &plan.target.remotes {
        writer.ensure_destination(remote)?;
    }
    let attempt = dir.join(format!("attempt-{}", random_id()?));
    private_dir(&attempt)?;
    crate::progress::items(0, plan.entries.len());
    for (index, entry) in plan.entries.iter().enumerate() {
        let completion = dir.join(format!("completed-{index:08}.json"));
        if completion.exists() {
            let manifest_path =
                verify_completion(&writer, &completion, dir, entry, plan.target.workers)?;
            crate::inventory::add_manifest(rclone, &manifest_path.to_string_lossy())?;
            crate::progress::items(index + 1, plan.entries.len());
            continue;
        }
        let archive_id = format!("reprocess-{}", random_id()?);
        let item = attempt.join(format!("{index:08}"));
        private_dir(&item)?;
        let durable_manifest = item.join("manifest.json");
        save_new(
            &item.join("receipt.json"),
            &serde_json::json!({
                "old_archive_id": entry.manifest.archive_id, "old_manifest_source": entry.source,
                "new_archive_id": archive_id, "target": plan.target,
                "new_manifest": durable_manifest, "state": "prepared"
            }),
        )?;
        let result: Result<()> = (|| {
            convert_one(&writer, rclone, entry, &plan.target, &item, &archive_id)?;
            crate::inventory::add_manifest(rclone, &durable_manifest.to_string_lossy())?;
            record_completion(&completion, entry, &durable_manifest)?;
            Ok(())
        })();
        save_new(
            &item.join("result.json"),
            &serde_json::json!({
                "new_archive_id": archive_id, "verified_and_indexed": result.is_ok(),
                "error": result.as_ref().err().map(|e| format!("{e:#}"))
            }),
        )?;
        result.with_context(|| {
            format!(
                "reprocessing stopped; original retained; attempt receipt: {}",
                item.display()
            )
        })?;
        crate::progress::items(index + 1, plan.entries.len());
    }
    crate::progress::finish();
    println!(
        "reprocess_complete={} elapsed_seconds={:.1} receipts={}",
        plan.entries.len(),
        started.elapsed().as_secs_f64(),
        attempt.display()
    );
    Ok(())
}

// File locks are kernel-owned: a crash releases ownership automatically. Never
// remove execution.lock: unlinking it would allow simultaneous lock owners.
fn lock_plan(dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("execution.lock"))?;
    file.try_lock()
        .map_err(|e| anyhow!("plan is already executing or cannot be locked: {e}"))?;
    Ok(file)
}

#[derive(Serialize, Deserialize)]
struct Completion {
    source_fingerprint: String,
    manifest_path: PathBuf,
    manifest_fingerprint: String,
}

fn record_completion(path: &Path, entry: &ReprocessEntry, manifest_path: &Path) -> Result<()> {
    let manifest: Manifest = read_json(manifest_path)?;
    save_new(
        path,
        &Completion {
            source_fingerprint: entry.fingerprint.clone(),
            manifest_path: manifest_path.canonicalize()?,
            manifest_fingerprint: manifest_fingerprint(&manifest)?,
        },
    )
}

fn validate_completion(path: &Path, dir: &Path, entry: &ReprocessEntry) -> Result<PathBuf> {
    let receipt: Completion =
        read_json(path).context("invalid completion receipt; originals retained")?;
    let manifest_path = receipt.manifest_path.canonicalize()?;
    if !manifest_path.starts_with(dir.canonicalize()?)
        || receipt.source_fingerprint != entry.fingerprint
    {
        bail!("completion receipt source/location mismatch; originals retained");
    }
    let manifest: Manifest = read_json(&manifest_path)?;
    validate_manifest(&manifest)?;
    if manifest_fingerprint(&manifest)? != receipt.manifest_fingerprint
        || manifest.original_size != entry.manifest.original_size
        || manifest.archive_id == entry.manifest.archive_id
    {
        bail!("completed manifest changed; originals retained");
    }
    Ok(manifest_path)
}

/// Read-only receipt lookup for explicit workspace recovery. This proves local
/// completion identity, not current remote availability: the caller must verify
/// replacement bytes and policy before publishing a mounted-file reference.
pub(crate) fn completed_reprocess_replacements(
    plan_path: &Path,
) -> Result<Vec<(Manifest, Manifest)>> {
    let plan = load_plan(plan_path)?;
    let canonical = plan_path.canonicalize()?;
    let dir = canonical.parent().context("plan directory missing")?;
    let mut replacements = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, entry) in plan.entries.iter().enumerate() {
        validate_manifest(&entry.manifest)?;
        if manifest_fingerprint(&entry.manifest)? != entry.fingerprint
            || !seen.insert(entry.fingerprint.clone())
        {
            bail!("reprocess source identity is invalid or duplicated");
        }
        let receipt = dir.join(format!("completed-{index:08}.json"));
        if !receipt.try_exists()? {
            continue;
        }
        let manifest_path = validate_completion(&receipt, dir, entry)?;
        let replacement: Manifest = read_json(&manifest_path)?;
        if !replacement.archive_id.starts_with("reprocess-")
            || replacement.archive_id.len() == "reprocess-".len()
            || !replacement
                .archive_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            bail!("replacement is not an independent reprocess archive");
        }
        for shard in &replacement.shards {
            let prefix =
                crate::utils::remote_join(&shard.remote, &format!("{}/", replacement.archive_id));
            let suffix = shard
                .object
                .strip_prefix(&prefix)
                .context("reprocess replacement contains borrowed source objects")?;
            if suffix.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains('\\')
                    || part.chars().any(char::is_control)
            }) {
                bail!("reprocess replacement contains borrowed source objects");
            }
        }
        replacements.push((entry.manifest.clone(), replacement));
    }
    Ok(replacements)
}

fn verify_completion(
    writer: &StorageWriter,
    path: &Path,
    dir: &Path,
    entry: &ReprocessEntry,
    workers: usize,
) -> Result<PathBuf> {
    let manifest_path = validate_completion(path, dir, entry)?;
    crate::commands::verify_with_storage(writer.reader(), &manifest_path.to_string_lossy(), true, workers)
        .context("completed replacement failed revalidation; originals retained, do not disconnect providers")?;
    Ok(manifest_path)
}

fn describe_changes(entries: &[ReprocessEntry], target: &PoolDefinition) -> Vec<String> {
    let mut notes = Vec::new();
    for entry in entries {
        let old: BTreeSet<_> = entry
            .manifest
            .shards
            .iter()
            .map(|s| s.remote.clone())
            .collect();
        let new: BTreeSet<_> = target.remotes.iter().cloned().collect();
        let added: Vec<_> = new.difference(&old).cloned().collect();
        let removed: Vec<_> = old.difference(&new).cloned().collect();
        let coding_changed = match &entry.manifest.coding {
            Some(c) => {
                target.parity_shards == 0
                    || c.data_shards != target.data_shards
                    || c.parity_shards != target.parity_shards
            }
            None => target.parity_shards != 0,
        };
        notes.push(format!("{}: added destinations {:?}; removed destinations {:?}; shard size changed: {}; K/M coding changed: {}. Destinations compare actual shard locations, not historical pool membership.",
            entry.manifest.archive_id, added, removed,
            target.shard_bytes().map_or(true, |b| entry.manifest.shard_size != b.get()), coding_changed));
    }
    notes.push("Full-copy replacement of every selected archive (not minimum movement). Workers/retries are execution-only knobs; changing them alone does not require rewriting existing data. Placement history is not recorded. Keep old providers accessible until every selected replacement is verified and indexed; unselected archives may still depend on them.".into());
    notes
}

/// Backend-injected conversion boundary: no inventory publication until this succeeds.
fn convert_one(
    writer: &StorageWriter,
    rclone: &str,
    entry: &ReprocessEntry,
    target: &PoolDefinition,
    item: &Path,
    archive_id: &str,
) -> Result<()> {
    let old = item.join("original-manifest.json");
    save_new(&old, &entry.manifest)?;
    let staging = tempfile::Builder::new().prefix("data-").tempdir_in(item)?;
    let source = staging.path().join("restored-file");
    crate::commands::get_with_storage(
        writer.reader(),
        &old.to_string_lossy(),
        &source,
        target.workers,
        target.retries,
    )?;
    crate::commands::put_with_storage(
        writer,
        rclone,
        &source,
        target.remotes.clone(),
        target.shard_mib()?,
        target.workers,
        target.placement,
        target.retries,
        target.data_shards,
        target.parity_shards,
        Some(archive_id.to_owned()),
        None,
    )?;
    let produced = append_suffix(&source, ".rpool.json");
    let mut manifest: Manifest = read_json(&produced)?;
    validate_manifest(&manifest)?;
    if manifest.archive_id != archive_id || manifest.original_size != entry.manifest.original_size {
        bail!("new archive identity/size mismatch");
    }
    crate::commands::verify_with_storage(
        writer.reader(),
        &produced.to_string_lossy(),
        true,
        target.workers,
    )?;
    // Restore exact display metadata without ever using it as a filesystem path.
    manifest
        .original_name
        .clone_from(&entry.manifest.original_name);
    validate_manifest(&manifest)?;
    crate::manifest::replicate_manifest_with_storage(
        writer,
        &manifest,
        &target.remotes,
        target.retries,
    )?;
    save_new(&item.join("manifest.json"), &manifest)?;
    Ok(())
}

#[cfg(test)]
#[path = "reprocess_tests.rs"]
mod tests;
