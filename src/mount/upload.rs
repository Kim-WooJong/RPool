//! Verified eligible-target uploads of a staged file (virtual drive sync and
//! incremental uploads). Resumable: a saved plan and journal sit beside the
//! staged copy; a completed manifest is re-verified and republished.
use crate::prelude::*;
use crate::utils::{append_suffix, hash_file_range, read_json};

fn require_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || is_reparse(&meta) {
        bail!("workspace requires a real directory: {}", path.display());
    }
    Ok(())
}

fn require_regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || is_reparse(&meta) {
        bail!("workspace refuses links/special files: {}", path.display());
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}

/// Upload `source` as archive `id` to the currently eligible targets. Returns
/// the verified manifest and the remotes its metadata was published to.
pub(super) fn upload_eligible_tracked(
    rclone: &str,
    policy: &PoolDefinition,
    pool: &str,
    source: &Path,
    id: &str,
) -> Result<(Manifest, Vec<String>)> {
    let status = super::capacity::CapacityStatus::inspect(
        &crate::storage::admin::RcloneAdmin::inherited(rclone),
        policy,
    )?;
    let size = fs::metadata(source)?.len();
    // Different eligible target sets use separate immutable upload journals.
    // Never resume a saved plan containing a now-excluded target.
    let key = blake3::hash(&serde_json::to_vec(&status.eligible)?)
        .to_hex()
        .to_string();
    let dir = source
        .parent()
        .context("staging parent missing")?
        .join(format!("eligible-{key}"));
    fs::create_dir_all(&dir)?;
    require_directory(&dir)?;
    let staged = dir.join(source.file_name().context("source filename missing")?);
    if !staged.exists() {
        fs::hard_link(source, &staged)?;
    }
    require_regular(&staged)?;
    if fs::metadata(&staged)?.len() != size
        || hash_file_range(&staged, 0, size)? != hash_file_range(source, 0, size)?
    {
        bail!("eligible upload staging identity mismatch");
    }
    let completed = append_suffix(&staged, ".rpool.json");
    if completed.exists() {
        let manifest: Manifest = read_json(&completed)?;
        crate::manifest::validate_manifest(&manifest)?;
        if manifest.archive_id != id
            || manifest.original_size != size
            || manifest
                .shards
                .iter()
                .any(|s| !status.eligible.contains(&s.remote))
        {
            bail!("completed upload does not match eligible transaction");
        }
        let publication =
            crate::manifest::publication_remotes(&manifest, &status.eligible, policy.placement);
        finalize_completed_upload(
            &crate::storage::writer::StorageWriter::for_pool(rclone, policy.native_crypt),
            &completed,
            &manifest,
            &publication,
            policy.workers,
            policy.retries,
        )?;
        return Ok((manifest, publication));
    }
    let plan_path = append_suffix(&staged, ".rpool.upload.json");
    let parity = if size == 0 { 0 } else { policy.parity_shards };
    let full_shard = policy.shard_bytes()?.get();
    let shard_size =
        crate::models::shard_size::shard_size_for(size, full_shard, policy.data_shards, parity);
    if plan_path.exists() {
        let plan: UploadPlan = read_json(&plan_path)?;
        if plan.archive_id != id
            || plan.source_size != size
            || plan.remotes != status.eligible
            || (plan.shard_size != shard_size && plan.shard_size != full_shard)
            || plan.placement != policy.placement
        {
            bail!("resume plan differs from eligible upload");
        }
        let journal_path = append_suffix(&staged, ".rpool.upload.state.json");
        let mut journal = crate::journal::load_or_create_upload_journal(&journal_path, &plan)?;
        // Only fully re-read, source-matching data earns admission credit.
        // Parity remains charged in full because put regenerates it.
        crate::journal::validate_upload_journal(
            &crate::storage::writer::StorageWriter::for_pool(rclone, policy.native_crypt),
            &staged,
            &plan,
            &mut journal,
        )?;
        let snapshot = crate::storage::admin::budget::BudgetSnapshot {
            targets: status.targets.clone(),
            rejected: vec![],
            observed_targets: vec![],
        };
        let mut budgets = snapshot.budgets();
        for shard in &plan.shards {
            if shard.kind == ShardKind::Data && journal.completed.contains_key(&shard.index) {
                continue;
            }
            let target = status
                .targets
                .iter()
                .find(|target| target.remote == shard.remote)
                .context("saved target excluded")?;
            let free = budgets
                .get_mut(&target.capacity_domain)
                .context("saved target has no quota")?;
            *free = free
                .checked_sub(shard.size)
                .context("insufficient quota to resume saved placement")?;
        }
        crate::placement::validate_resilient_plan(rclone, &plan)?;
    } else {
        status.check_upload(policy, size)?;
        let coding = (size > 0 && policy.parity_shards > 0).then(|| Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: policy.data_shards,
            parity_shards: policy.parity_shards,
            stripe_size: EC_STRIPE_SIZE,
        });
        let plan = crate::planning::build_upload_plan(
            rclone,
            size,
            shard_size,
            id.into(),
            status.eligible.clone(),
            policy.placement,
            coding,
        )?;
        super::namespace::durable_json(&plan_path, &plan)?;
    }
    crate::commands::put_with_storage(
        &crate::storage::writer::StorageWriter::for_pool(rclone, policy.native_crypt),
        rclone,
        &staged,
        status.eligible.clone(),
        policy.shard_mib()?,
        policy.workers,
        policy.placement,
        policy.retries,
        policy.data_shards,
        if size == 0 { 0 } else { policy.parity_shards },
        Some(id.into()),
        Some(pool.into()),
    )?;
    let path = append_suffix(&staged, ".rpool.json");
    // put read every shard back already; this re-check only stats objects it
    // proved in this process and fully reads anything else.
    crate::commands::reverify_with_storage(
        &crate::storage::reader::StorageReader::rclone(rclone),
        &path.to_string_lossy(),
        policy.workers,
    )?;
    let manifest: Manifest = read_json(&path)?;
    let publication =
        crate::manifest::publication_remotes(&manifest, &status.eligible, policy.placement);
    Ok((manifest, publication))
}

fn finalize_completed_upload(
    storage: &crate::storage::writer::StorageWriter,
    path: &Path,
    manifest: &Manifest,
    remotes: &[String],
    workers: usize,
    retries: u32,
) -> Result<()> {
    crate::commands::reverify_with_storage(storage.reader(), &path.to_string_lossy(), workers)?;
    // put persists the local manifest BEFORE remote metadata replication. A
    // local completed manifest is not proof that publication finished.
    crate::manifest::replicate_manifest_with_storage(storage, manifest, remotes, retries)?;
    Ok(())
}
