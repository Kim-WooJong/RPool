use crate::prelude::*;
use crate::erasure::validate_rs_counts;
use crate::manifest::{coding_group_count, content_root_v1, content_root_v2, data_shards};

pub(crate) fn validate_manifest(manifest: &Manifest) -> Result<()> {
    match manifest.version {
        1 => validate_manifest_v1(manifest),
        2 => validate_manifest_v2(manifest),
        other => bail!("unsupported manifest version: {other}"),
    }
}

pub(crate) fn validate_manifest_v1(manifest: &Manifest) -> Result<()> {
    let mut expected_offset = 0u64;
    for (position, shard) in manifest.shards.iter().enumerate() {
        if shard.index as usize != position {
            bail!("manifest shard indexes are not contiguous");
        }
        if shard.offset != expected_offset {
            bail!("manifest shard offsets are not contiguous");
        }
        expected_offset = expected_offset
            .checked_add(shard.size)
            .ok_or_else(|| anyhow!("manifest size overflow"))?;
    }
    if expected_offset != manifest.original_size {
        bail!(
            "manifest size mismatch: shards={}, original={}",
            expected_offset,
            manifest.original_size
        );
    }
    if content_root_v1(&manifest.shards) != manifest.content_root_blake3 {
        bail!("manifest content-root checksum mismatch");
    }
    Ok(())
}

pub(crate) fn validate_manifest_v2(manifest: &Manifest) -> Result<()> {
    for (position, shard) in manifest.shards.iter().enumerate() {
        if shard.index as usize != position {
            bail!("manifest physical shard indexes are not contiguous");
        }
    }

    let data = data_shards(manifest);
    let mut expected_offset = 0u64;
    for (position, shard) in data.iter().enumerate() {
        if shard.index as usize != position {
            bail!("manifest data shard indexes are not contiguous from zero");
        }
        if shard.offset != expected_offset {
            bail!("manifest data shard offsets are not contiguous");
        }
        expected_offset = expected_offset
            .checked_add(shard.size)
            .ok_or_else(|| anyhow!("manifest size overflow"))?;
    }
    if expected_offset != manifest.original_size {
        bail!(
            "manifest size mismatch: data_shards={}, original={}",
            expected_offset,
            manifest.original_size
        );
    }

    if let Some(coding) = &manifest.coding {
        if coding.algorithm != RS_ALGORITHM {
            bail!("unsupported erasure coding algorithm: {}", coding.algorithm);
        }
        validate_rs_counts(coding.data_shards, coding.parity_shards)?;
        if coding.stripe_size == 0 {
            bail!("coding stripe_size must be greater than zero");
        }
        let groups = coding_group_count(data.len(), coding.data_shards);
        let parity: Vec<&Shard> = manifest
            .shards
            .iter()
            .filter(|s| s.kind == ShardKind::Parity)
            .collect();
        if parity.len() != groups * coding.parity_shards {
            bail!(
                "manifest parity count mismatch: got {}, expected {}",
                parity.len(),
                groups * coding.parity_shards
            );
        }

        for (position, shard) in data.iter().enumerate() {
            let expected_group = position / coding.data_shards;
            let expected_slot = position % coding.data_shards;
            if shard.group as usize != expected_group || shard.slot as usize != expected_slot {
                bail!("data shard {} has inconsistent group/slot metadata", shard.index);
            }
        }

        for group in 0..groups {
            for parity_index in 0..coding.parity_shards {
                let expected_index = data.len() + group * coding.parity_shards + parity_index;
                let shard = manifest
                    .shards
                    .get(expected_index)
                    .ok_or_else(|| anyhow!("missing parity shard index {expected_index}"))?;
                if shard.kind != ShardKind::Parity
                    || shard.group as usize != group
                    || shard.slot as usize != coding.data_shards + parity_index
                    || shard.size != manifest.shard_size
                {
                    bail!("parity shard {expected_index} has inconsistent metadata");
                }
            }
        }
    } else if manifest
        .shards
        .iter()
        .any(|s| s.kind == ShardKind::Parity)
    {
        bail!("manifest contains parity shards but no coding metadata");
    }

    if content_root_v2(
        manifest.original_size,
        manifest.shard_size,
        &manifest.coding,
        &manifest.shards,
    ) != manifest.content_root_blake3
    {
        bail!("manifest content-root checksum mismatch");
    }
    Ok(())
}
