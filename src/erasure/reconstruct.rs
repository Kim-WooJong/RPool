//! Data-shard recovery for downloads (`commands::get_hedged`).
use crate::manifest::data_shards;
use crate::prelude::*;
use crate::utils::{hash_file_range, read_exact_at, write_all_at};

/// Rebuild `missing_data` shards of `group` in place inside `output` (the
/// partially downloaded file) from the present data shards and the
/// `local_parity` files `(slot, path)`. Fails if fewer parity shards than
/// missing shards are available or a rebuilt shard fails its BLAKE3 check.
pub(crate) fn reconstruct_group(
    manifest: &Manifest,
    coding: &Coding,
    group: u32,
    missing_data: &[Shard],
    output: &Path,
    local_parity: &[(usize, PathBuf)],
) -> Result<()> {
    let needed = missing_data.len();
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
    for (slot, path) in local_parity {
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

        for (slot, block) in blocks.iter_mut().enumerate().take(coding.data_shards) {
            let data_index = group_data_start + slot;
            if data_index >= real_data_count {
                *block = Some(vec![0u8; chunk_len]);
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
            *block = Some(buf);
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
