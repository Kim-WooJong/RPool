use crate::manifest::{
    content_root_v2, manifest_remotes, replicate_manifest_with_storage, validate_manifest,
};
use crate::planning::manifest_single_provider_failure_safety;
use crate::prelude::*;
use crate::storage::writer::StorageWriter;
use crate::utils::{relative_remote_object, remote_join, save_json_atomic};

#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_manifest(
    rclone: &str,
    manifest: &Manifest,
    from: &str,
    to: &str,
    output: &Path,
    workers: usize,
    retries: u32,
    dry_run: bool,
    delete_source: bool,
    allow_risky: bool,
) -> Result<Manifest> {
    drain_manifest_with_storage(
        &StorageWriter::rclone(rclone),
        manifest,
        from,
        to,
        output,
        workers,
        retries,
        dry_run,
        delete_source,
        allow_risky,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_manifest_with_storage(
    storage: &StorageWriter,
    manifest: &Manifest,
    from: &str,
    to: &str,
    output: &Path,
    workers: usize,
    retries: u32,
    dry_run: bool,
    delete_source: bool,
    allow_risky: bool,
) -> Result<Manifest> {
    if from == to {
        bail!("source and destination provider are identical");
    }
    if delete_source
        && (manifest.archive_id.starts_with("virtual-")
            || manifest.archive_id.starts_with("peer-v7-"))
    {
        bail!("cannot delete virtual-drive archive sources: immutable shared revisions may still reference them; omit --delete-source or reprocess into a fresh archive");
    }
    storage.ensure_destination(to)?;
    if !delete_source {
        storage.ensure_destination(from).with_context(|| {
            "the source provider would receive an updated manifest replica; use a crypt source remote or migrate with --delete-source"
        })?;
    }
    if manifest.version != 2 {
        bail!(
            "provider drain requires a v2 manifest; legacy v1 manifests remain supported for compatible restore/verify/status operations"
        );
    }

    let targets: Vec<Shard> = manifest
        .shards
        .iter()
        .filter(|shard| shard.remote == from)
        .cloned()
        .collect();
    if targets.is_empty() {
        bail!("manifest has no shards assigned to provider: {from}");
    }

    let mut preview = manifest.clone();
    for shard in &mut preview.shards {
        if shard.remote == from {
            let relative = relative_remote_object(&shard.remote, &shard.object)?;
            shard.remote = to.to_string();
            shard.object = remote_join(to, &relative);
        }
    }
    preview.content_root_blake3 = content_root_v2(
        preview.original_size,
        preview.shard_size,
        &preview.coding,
        &preview.shards,
    );
    validate_manifest(&preview)?;
    if let Some(coding) = &preview.coding {
        let (safe, max_on_provider) = manifest_single_provider_failure_safety(&preview, coding);
        if safe != crate::planning::FailureSafety::Safe && !allow_risky {
            bail!(
                "drain cannot establish single-provider failure safety ({safe}): at least {max_on_provider} shard(s) from one coding group share a configured remote; physical domains may overlap further, parity budget={}; rerun with --allow-risky only if this is intentional",
                coding.parity_shards
            );
        }
    }

    if dry_run {
        println!("archive_id={}", manifest.archive_id);
        println!("from={from}");
        println!("to={to}");
        println!("shards={}", targets.len());
        println!("delete_source={delete_source}");
        for shard in &targets {
            let relative = relative_remote_object(&shard.remote, &shard.object)?;
            println!(
                "move={}:{} -> {}",
                shard.index,
                shard.object,
                remote_join(to, &relative)
            );
        }
        return Ok(preview);
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;
    let copied: Vec<Result<(u32, String)>> = pool.install(|| {
        targets
            .par_iter()
            .map(|shard| {
                let relative = relative_remote_object(&shard.remote, &shard.object)?;
                let destination = remote_join(to, &relative);
                storage.copy_verified(shard, &destination, retries)?;
                let mut verify = shard.clone();
                verify.remote = to.to_string();
                verify.object = destination.clone();
                storage.reader().verify(&verify, true)?;
                Ok((shard.index, destination))
            })
            .collect()
    });

    let mut destinations = BTreeMap::new();
    for result in copied {
        let (index, destination) = result?;
        destinations.insert(index, destination);
    }

    let mut updated = preview;
    for shard in &mut updated.shards {
        if let Some(destination) = destinations.get(&shard.index) {
            shard.object = destination.clone();
        }
    }
    updated.content_root_blake3 = content_root_v2(
        updated.original_size,
        updated.shard_size,
        &updated.coding,
        &updated.shards,
    );
    validate_manifest(&updated)?;
    save_json_atomic(output, &updated)?;

    let mut replica_remotes = manifest_remotes(&updated);
    if !delete_source && !replica_remotes.iter().any(|remote| remote == from) {
        replica_remotes.push(from.to_string());
    }
    for target in replicate_manifest_with_storage(storage, &updated, &replica_remotes, retries)? {
        eprintln!("[manifest] {target}");
    }

    if delete_source {
        for shard in &targets {
            storage.delete(&shard.object)?;
        }
        let old_manifest = remote_join(from, &format!("{}/manifest.json", manifest.archive_id));
        if let Err(error) = storage.delete(&old_manifest) {
            eprintln!("[warning] could not remove old manifest replica {old_manifest}: {error:#}");
        }
    }

    Ok(updated)
}
