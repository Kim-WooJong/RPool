use crate::manifest::data_shards;
use crate::prelude::*;
use crate::progress;
use crate::storage::writer::StorageWriter;
use crate::utils::{hash_file_range, read_exact_at};

pub(crate) fn repair_manifest_with_storage(
    storage: &StorageWriter,
    manifest: &Manifest,
    probes: &[(Shard, Probe)],
    retries: u32,
    dry_run: bool,
    selected_groups: Option<&BTreeSet<u32>>,
) -> Result<usize> {
    let in_scope = |shard: &Shard| {
        selected_groups
            .map(|groups| groups.contains(&shard.group))
            .unwrap_or(true)
    };

    let transport_errors = probes
        .iter()
        .filter(|(shard, probe)| in_scope(shard) && matches!(probe, Probe::Error(_)))
        .count();
    if transport_errors > 0 {
        bail!(
            "refusing repair while {transport_errors} selected shard probe(s) have transport/provider errors; resolve provider health first so an unknown state is not treated as an erasure"
        );
    }

    let bad: Vec<Shard> = probes
        .iter()
        .filter(|(shard, probe)| {
            in_scope(shard)
                && matches!(
                    probe,
                    Probe::Missing | Probe::BadSize { .. } | Probe::Corrupt { .. }
                )
        })
        .map(|(shard, _)| shard.clone())
        .collect();
    if bad.is_empty() {
        return Ok(0);
    }

    let Some(coding) = &manifest.coding else {
        bail!(
            "archive has no Reed-Solomon coding; {} bad shard(s) cannot be regenerated",
            bad.len()
        );
    };

    let groups: BTreeSet<u32> = bad.iter().map(|shard| shard.group).collect();
    progress::items(0, bad.len());
    let mut repaired = 0usize;
    for group in groups {
        let group_bad: Vec<Shard> = bad
            .iter()
            .filter(|shard| shard.group == group)
            .cloned()
            .collect();
        if dry_run {
            println!(
                "repair_group={} shards={} dry_run=true",
                group,
                group_bad.len()
            );
            repaired += group_bad.len();
            progress::items(repaired, bad.len());
            continue;
        }
        repaired += repair_group(storage, manifest, coding, group, &group_bad, retries)?;
        progress::items(repaired, bad.len());
    }
    progress::finish();
    Ok(repaired)
}

fn repair_group(
    storage: &StorageWriter,
    manifest: &Manifest,
    coding: &Coding,
    group: u32,
    bad: &[Shard],
    retries: u32,
) -> Result<usize> {
    let bad_indexes: BTreeSet<u32> = bad.iter().map(|shard| shard.index).collect();
    let group_shards: Vec<Shard> = manifest
        .shards
        .iter()
        .filter(|shard| shard.group == group)
        .cloned()
        .collect();
    let real_data = group_shards
        .iter()
        .filter(|shard| shard.kind == ShardKind::Data)
        .count();
    let virtual_zero = coding.data_shards.saturating_sub(real_data);
    let healthy_physical = group_shards.len().saturating_sub(bad.len());
    if healthy_physical + virtual_zero < coding.data_shards {
        bail!(
            "group {group} is unrecoverable: healthy_physical={healthy_physical} virtual_zero={virtual_zero} required={}",
            coding.data_shards
        );
    }

    let temp_owner = tempfile::tempdir()?;
    let temp_root = temp_owner.path();

    let mut local_paths: BTreeMap<u32, PathBuf> = BTreeMap::new();
    for shard in &group_shards {
        if bad_indexes.contains(&shard.index) {
            continue;
        }
        let path = temp_root.join(format!("healthy-{:08}.bin", shard.index));
        storage
            .reader()
            .download(shard, &path, retries, false)
            .with_context(|| {
                format!(
                    "cannot fetch healthy shard {:08} needed to repair group {group}",
                    shard.index
                )
            })?;
        local_paths.insert(shard.index, path);
    }

    let output_paths =
        reconstruct_group_files(manifest, coding, group, bad, &local_paths, temp_root)?;
    for shard in bad {
        let path = output_paths
            .get(&shard.index)
            .ok_or_else(|| anyhow!("missing repaired path"))?;
        storage.write_file(path, 0, shard, retries)?;
        eprintln!("[repair] {:08} {}", shard.index, shard.remote);
    }

    Ok(bad.len())
}

/// Rebuilds the `bad` shards of `group` from verified local copies of every
/// other shard of that group (`healthy`: shard index -> local file) into
/// `out_dir`. Each output is checked against the manifest size and BLAKE3
/// before it is returned. Nothing is written to storage.
pub(crate) fn reconstruct_group_files(
    manifest: &Manifest,
    coding: &Coding,
    group: u32,
    bad: &[Shard],
    healthy: &BTreeMap<u32, PathBuf>,
    out_dir: &Path,
) -> Result<BTreeMap<u32, PathBuf>> {
    let bad_indexes: BTreeSet<u32> = bad.iter().map(|shard| shard.index).collect();
    let group_shards: Vec<Shard> = manifest
        .shards
        .iter()
        .filter(|shard| shard.group == group)
        .cloned()
        .collect();
    let local_paths = healthy;
    let temp_root = out_dir;
    let mut output_paths: BTreeMap<u32, PathBuf> = BTreeMap::new();
    let mut output_files: BTreeMap<u32, BufWriter<File>> = BTreeMap::new();
    for shard in bad {
        let path = temp_root.join(format!("repaired-{:08}.bin", shard.index));
        output_files.insert(shard.index, BufWriter::new(File::create(&path)?));
        output_paths.insert(shard.index, path);
    }

    let rs = ReedSolomon::new(coding.data_shards, coding.parity_shards)
        .map_err(|e| anyhow!("cannot initialize Reed-Solomon decoder: {e}"))?;
    let all_data = data_shards(manifest);
    let group_data_start = group as usize * coding.data_shards;

    let mut stripe_offset = 0u64;
    while stripe_offset < manifest.shard_size {
        let chunk_len =
            (manifest.shard_size - stripe_offset).min(coding.stripe_size.max(1) as u64) as usize;
        let mut blocks: Vec<Option<Vec<u8>>> = (0..coding.data_shards + coding.parity_shards)
            .map(|_| None)
            .collect();

        for (slot, block) in blocks.iter_mut().enumerate().take(coding.data_shards) {
            let data_index = group_data_start + slot;
            if data_index >= all_data.len() {
                *block = Some(vec![0u8; chunk_len]);
                continue;
            }
            let shard = all_data[data_index];
            if bad_indexes.contains(&shard.index) {
                continue;
            }
            let path = local_paths
                .get(&shard.index)
                .ok_or_else(|| anyhow!("missing local healthy data shard {}", shard.index))?;
            let file = File::open(path)?;
            let mut buf = vec![0u8; chunk_len];
            if stripe_offset < shard.size {
                let read_len = (shard.size - stripe_offset).min(chunk_len as u64) as usize;
                read_exact_at(&file, &mut buf[..read_len], stripe_offset)?;
            }
            *block = Some(buf);
        }

        for parity_index in 0..coding.parity_shards {
            let slot = coding.data_shards + parity_index;
            let shard = group_shards
                .iter()
                .find(|shard| shard.kind == ShardKind::Parity && shard.slot as usize == slot)
                .ok_or_else(|| anyhow!("missing parity metadata for group {group} slot {slot}"))?;
            if bad_indexes.contains(&shard.index) {
                continue;
            }
            let path = local_paths
                .get(&shard.index)
                .ok_or_else(|| anyhow!("missing local healthy parity shard {}", shard.index))?;
            let file = File::open(path)?;
            let mut buf = vec![0u8; chunk_len];
            read_exact_at(&file, &mut buf, stripe_offset)?;
            blocks[slot] = Some(buf);
        }

        rs.reconstruct(&mut blocks)
            .map_err(|e| anyhow!("Reed-Solomon reconstruction failed for group {group}: {e}"))?;

        for shard in bad {
            let block = blocks[shard.slot as usize]
                .as_ref()
                .ok_or_else(|| anyhow!("decoder did not reconstruct slot {}", shard.slot))?;
            let write_len = if shard.kind == ShardKind::Data {
                if stripe_offset >= shard.size {
                    0
                } else {
                    (shard.size - stripe_offset).min(chunk_len as u64) as usize
                }
            } else {
                chunk_len
            };
            if write_len > 0 {
                output_files
                    .get_mut(&shard.index)
                    .ok_or_else(|| anyhow!("missing repair writer for shard {}", shard.index))?
                    .write_all(&block[..write_len])?;
            }
        }
        stripe_offset += chunk_len as u64;
    }

    for writer in output_files.values_mut() {
        writer.flush()?;
    }
    drop(output_files);

    for shard in bad {
        let path = output_paths
            .get(&shard.index)
            .ok_or_else(|| anyhow!("missing repaired path"))?;
        let size = fs::metadata(path)?.len();
        if size != shard.size {
            bail!(
                "repaired shard {} size mismatch: {size} != {}",
                shard.index,
                shard.size
            );
        }
        let hash = hash_file_range(path, 0, shard.size)?;
        if hash != shard.blake3 {
            bail!("repaired shard {} failed BLAKE3 validation", shard.index);
        }
    }
    Ok(output_paths)
}
