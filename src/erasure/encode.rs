use crate::planning::shard_from_plan;
use crate::prelude::*;
use crate::storage::writer::StorageWriter;

pub(crate) fn upload_parity_groups(
    storage: &StorageWriter,
    source: &Path,
    plan: &UploadPlan,
    coding: &Coding,
    workers: usize,
    retries: u32,
) -> Result<Vec<Shard>> {
    let parity_plans: Vec<PlanShard> = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Parity)
        .cloned()
        .collect();
    if parity_plans.is_empty() {
        return Ok(Vec::new());
    }

    let group_count = parity_plans
        .iter()
        .map(|s| s.group)
        .max()
        .map(|v| v as usize + 1)
        .unwrap_or(0);
    let temp_owner = tempfile::tempdir()?;
    let temp_root = temp_owner.path();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;
    let mut result = Vec::with_capacity(parity_plans.len());

    for group in 0..group_count {
        eprintln!("[ec] encoding group {}/{}", group + 1, group_count);
        let generated = generate_parity_group(source, plan, coding, group as u32, &temp_root)?;

        let uploads: Vec<Result<Shard>> = pool.install(|| {
            generated
                .par_iter()
                .map(|item| {
                    let shard = shard_from_plan(&item.plan, item.blake3.clone());
                    storage.write_file(&item.path, 0, &shard, retries)?;
                    Ok(shard_from_plan(&item.plan, item.blake3.clone()))
                })
                .collect()
        });

        for upload in uploads {
            result.push(upload?);
        }
        for item in generated {
            fs::remove_file(item.path).ok();
        }
    }

    Ok(result)
}

pub(crate) fn generate_parity_group(
    source: &Path,
    plan: &UploadPlan,
    coding: &Coding,
    group: u32,
    temp_root: &Path,
) -> Result<Vec<GeneratedParity>> {
    let rs = ReedSolomon::new(coding.data_shards, coding.parity_shards)
        .map_err(|e| anyhow!("cannot initialize Reed-Solomon encoder: {e}"))?;

    let parity_plans: Vec<PlanShard> = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Parity && s.group == group)
        .cloned()
        .collect();
    if parity_plans.len() != coding.parity_shards {
        bail!(
            "upload plan group {} contains {} parity shards, expected {}",
            group,
            parity_plans.len(),
            coding.parity_shards
        );
    }

    let group_dir = temp_root.join(format!("g{group:08}"));
    fs::create_dir_all(&group_dir)?;

    let mut paths = Vec::with_capacity(coding.parity_shards);
    let mut writers = Vec::with_capacity(coding.parity_shards);
    let mut hashers = Vec::with_capacity(coding.parity_shards);
    for parity_index in 0..coding.parity_shards {
        let path = group_dir.join(format!("p{parity_index:03}.bin"));
        writers.push(BufWriter::new(File::create(&path)?));
        hashers.push(Hasher::new());
        paths.push(path);
    }

    let mut input = File::open(source)?;
    let data_count = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Data)
        .count();
    let group_start = group as usize * coding.data_shards;

    let mut stripe_offset = 0u64;
    while stripe_offset < plan.shard_size {
        let chunk_len =
            (plan.shard_size - stripe_offset).min(coding.stripe_size.max(1) as u64) as usize;
        let mut blocks = vec![vec![0u8; chunk_len]; coding.data_shards + coding.parity_shards];

        for slot in 0..coding.data_shards {
            let data_index = group_start + slot;
            if data_index >= data_count {
                continue;
            }
            let shard = plan
                .shards
                .iter()
                .find(|s| s.kind == ShardKind::Data && s.index as usize == data_index)
                .ok_or_else(|| anyhow!("missing data shard {data_index} in upload plan"))?;
            if stripe_offset >= shard.size {
                continue;
            }
            let read_len = (shard.size - stripe_offset).min(chunk_len as u64) as usize;
            input.seek(SeekFrom::Start(shard.offset + stripe_offset))?;
            input.read_exact(&mut blocks[slot][..read_len])?;
        }

        rs.encode(&mut blocks)
            .map_err(|e| anyhow!("Reed-Solomon encoding failed for group {group}: {e}"))?;

        for parity_index in 0..coding.parity_shards {
            let block = &blocks[coding.data_shards + parity_index];
            writers[parity_index].write_all(block)?;
            hashers[parity_index].update(block);
        }

        stripe_offset += chunk_len as u64;
    }

    for writer in &mut writers {
        writer.flush()?;
    }
    drop(writers);

    let mut generated = Vec::with_capacity(coding.parity_shards);
    for parity_index in 0..coding.parity_shards {
        let slot = (coding.data_shards + parity_index) as u16;
        let plan_shard = parity_plans
            .iter()
            .find(|s| s.slot == slot)
            .ok_or_else(|| anyhow!("missing parity slot {slot} in upload plan"))?
            .clone();
        generated.push(GeneratedParity {
            plan: plan_shard,
            path: paths[parity_index].clone(),
            blake3: hashers[parity_index].finalize().to_hex().to_string(),
        });
    }

    Ok(generated)
}
