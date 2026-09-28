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
}

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random identifier: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

// Create-only, fsynced receipts avoid losing discovery information on interrupted writes.
fn save_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn storage_bytes(size: u64, target: &PoolDefinition) -> Result<u64> {
    let shard = target
        .shard_mib
        .checked_mul(1024 * 1024)
        .context("shard size overflow")?;
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
    let plan = ReprocessPlan {
        version: 1, operation_id, plan_path: dir.join("plan.json"), target, entries,
        input_bytes, new_storage_bytes, download_bytes, upload_bytes, estimated_seconds,
        estimate_note: "Approximate sequential transfer time using user-supplied aggregate rates. Includes upload readback and final full verification; excludes metadata, encryption overhead, disk/CPU time, retries and degraded-source recovery. Originals remain stored; target capacity is additional. Unknown without both rates.".into(),
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

pub(crate) fn execute_plan(rclone: &str, plan_path: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    let plan_path = plan_path.canonicalize()?;
    let dir = plan_path.parent().context("plan directory missing")?;
    let bytes = fs::read(&plan_path)?;
    let expected: String = read_json(&dir.join("plan.fingerprint.json"))?;
    if blake3::hash(&bytes).to_hex().as_str() != expected {
        bail!("saved plan changed; create a new plan");
    }
    let plan: ReprocessPlan = serde_json::from_slice(&bytes)?;
    if plan.version != 1 || plan.plan_path.canonicalize()? != plan_path {
        bail!("invalid saved plan location/version");
    }
    super::validate_pool(&plan.target)?;
    // Check every source before any remote mutation; restore from the frozen snapshot below.
    for entry in &plan.entries {
        validate_manifest(&entry.manifest)?;
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
    let writer = StorageWriter::rclone(rclone);
    for remote in &plan.target.remotes {
        writer.ensure_destination(remote)?;
    }
    let attempt = dir.join(format!("attempt-{}", random_id()?));
    private_dir(&attempt)?;
    crate::progress::items(0, plan.entries.len());
    for (index, entry) in plan.entries.iter().enumerate() {
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
        target.shard_mib,
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
