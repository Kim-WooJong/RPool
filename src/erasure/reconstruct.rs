use crate::manifest::data_shards;
use crate::prelude::*;
use crate::storage::reader::{is_restore_unavailable, StorageReader};
use crate::utils::{hash_file_range, read_exact_at, write_all_at};

pub(crate) fn reconstruct_group(
    reader: &StorageReader,
    manifest: &Manifest,
    coding: &Coding,
    group: u32,
    missing_data: &[Shard],
    output: &Path,
    retries: u32,
    workers: usize,
) -> Result<()> {
    let needed = missing_data.len();
    let parity: Vec<Shard> = manifest
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Parity && s.group == group)
        .cloned()
        .collect();

    // Unique operation-owned directory; declared before handles so Windows closes
    // every parity file before TempDir cleanup on success or early error.
    let temporary = tempfile::Builder::new()
        .prefix("rpool-restore-")
        .tempdir()?;
    let temp_root = temporary.path();

    let mut local_parity: Vec<(usize, PathBuf)> = Vec::new();
    // Start up to the number needed. Failed candidates are replaced without
    // waiting for another healthy candidate; all in-flight work is joined.
    let mut candidates = parity.into_iter();
    let initial: Vec<_> = candidates.by_ref().take(needed).collect();
    crate::storage::scheduler::run(
        initial,
        workers,
        retries,
        |s| crate::storage::scheduler::remote_key(&s.remote),
        |shard| {
            let path = temp_root.join(format!("p{:03}.bin", shard.slot));
            reader.download(shard, &path, 1, false)?;
            Ok(path)
        },
        crate::storage::scheduler::read_retry,
        |shard, result| {
            match result {
                Ok(path) => local_parity.push((shard.slot as usize, path)),
                Err(error) if is_restore_unavailable(&error) => {
                    eprintln!(
                        "[degraded] parity shard g{group:08}/s{:03} unavailable: {error:#}",
                        shard.slot
                    );
                    return Ok(candidates.next().into_iter().collect());
                }
                Err(error) => return Err(error),
            }
            Ok(vec![])
        },
    )?;

    if local_parity.len() < needed {
        bail!(
            "group {} requires {} parity shards but only {} valid parity shards were available",
            group,
            needed,
            local_parity.len()
        );
    }

    let rs = ReedSolomon::new(coding.data_shards, coding.parity_shards)
        .map_err(|e| anyhow!("cannot initialize Reed-Solomon decoder: {e}"))?;
    let output_file = OpenOptions::new().read(true).write(true).open(output)?;

    let mut parity_files: Vec<Option<File>> = (0..coding.parity_shards).map(|_| None).collect();
    for (slot, path) in &local_parity {
        let parity_index = slot - coding.data_shards;
        parity_files[parity_index] = Some(File::open(path)?);
    }

    let missing_slots: BTreeSet<usize> = missing_data.iter().map(|s| s.slot as usize).collect();
    let group_data_start = group as usize * coding.data_shards;
    let all_data = data_shards(manifest);
    let real_data_count = all_data.len();

    let mut stripe_offset = 0u64;
    while stripe_offset < manifest.shard_size {
        let chunk_len =
            (manifest.shard_size - stripe_offset).min(coding.stripe_size.max(1) as u64) as usize;
        let mut blocks: Vec<Option<Vec<u8>>> = (0..coding.data_shards + coding.parity_shards)
            .map(|_| None)
            .collect();

        for slot in 0..coding.data_shards {
            let data_index = group_data_start + slot;
            if data_index >= real_data_count {
                blocks[slot] = Some(vec![0u8; chunk_len]);
                continue;
            }
            if missing_slots.contains(&slot) {
                continue;
            }

            let shard = all_data[data_index];
            let mut buf = vec![0u8; chunk_len];
            if stripe_offset < shard.size {
                let read_len = (shard.size - stripe_offset).min(chunk_len as u64) as usize;
                read_exact_at(
                    &output_file,
                    &mut buf[..read_len],
                    shard.offset + stripe_offset,
                )?;
            }
            blocks[slot] = Some(buf);
        }

        for parity_index in 0..coding.parity_shards {
            if let Some(file) = &parity_files[parity_index] {
                let mut buf = vec![0u8; chunk_len];
                read_exact_at(file, &mut buf, stripe_offset)?;
                blocks[coding.data_shards + parity_index] = Some(buf);
            }
        }

        rs.reconstruct(&mut blocks)
            .map_err(|e| anyhow!("Reed-Solomon reconstruction failed for group {group}: {e}"))?;

        for shard in missing_data {
            let slot = shard.slot as usize;
            let block = blocks[slot]
                .as_ref()
                .ok_or_else(|| anyhow!("decoder did not reconstruct data slot {slot}"))?;
            if stripe_offset < shard.size {
                let write_len = (shard.size - stripe_offset).min(chunk_len as u64) as usize;
                write_all_at(
                    &output_file,
                    &block[..write_len],
                    shard.offset + stripe_offset,
                )?;
            }
        }

        stripe_offset += chunk_len as u64;
    }

    for shard in missing_data {
        let hash = hash_file_range(output, shard.offset, shard.size)?;
        if hash != shard.blake3 {
            bail!(
                "reconstructed data shard {:08} failed BLAKE3 validation",
                shard.index
            );
        }
        eprintln!("[recover] {:08} group={}", shard.index, group);
    }

    Ok(())
}
