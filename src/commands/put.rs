use crate::erasure::{upload_parity_groups, validate_rs_counts};
use crate::journal::{load_or_create_upload_journal, record_upload_shard, validate_upload_journal};
use crate::manifest::{content_root_v2, replicate_manifest_with_storage, validate_manifest};
use crate::planning::{build_upload_plan, warn_plan_failure_domains};
use crate::prelude::*;
use crate::storage::{source::UploadSource, upload_one_data_shard, writer::StorageWriter};
use crate::utils::{
    append_suffix, ensure_positive, make_archive_id, now_unix, read_json, save_json_atomic,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn put(
    rclone: &str,
    source: &Path,
    remotes: Vec<String>,
    shard_mib: u64,
    workers: usize,
    placement: Placement,
    retries: u32,
    data_shards: usize,
    parity_shards: usize,
    explicit_id: Option<String>,
    pool_name: Option<String>,
) -> Result<()> {
    let original = source.canonicalize()?;
    put_with_storage(
        &StorageWriter::rclone(rclone),
        rclone,
        &original,
        remotes,
        shard_mib,
        workers,
        placement,
        retries,
        data_shards,
        parity_shards,
        explicit_id,
        pool_name,
    )?;
    let manifest_source = append_suffix(&original, ".rpool.json")
        .to_string_lossy()
        .into_owned();
    if let Err(error) = crate::inventory::add_manifest(rclone, &manifest_source) {
        eprintln!("[inventory] archive completed but local index update failed: {error:#}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn put_with_storage(
    storage: &StorageWriter,
    rclone: &str,
    source: &Path,
    remotes: Vec<String>,
    shard_mib: u64,
    workers: usize,
    placement: Placement,
    retries: u32,
    data_shards: usize,
    parity_shards: usize,
    explicit_id: Option<String>,
    pool_name: Option<String>,
) -> Result<()> {
    ensure_positive(workers, "workers")?;
    if shard_mib == 0 {
        bail!("shard-mib must be greater than zero");
    }
    if remotes.is_empty() {
        bail!("at least one --remote is required");
    }
    for remote in &remotes {
        storage.ensure_destination(remote)?;
    }

    let coding = if parity_shards > 0 {
        validate_rs_counts(data_shards, parity_shards)?;
        Some(Coding {
            algorithm: RS_ALGORITHM.to_string(),
            data_shards,
            parity_shards,
            stripe_size: EC_STRIPE_SIZE,
        })
    } else {
        None
    };

    let source = source
        .canonicalize()
        .with_context(|| format!("cannot open source: {}", source.display()))?;
    let meta = fs::metadata(&source)?;
    if !meta.is_file() {
        bail!("put currently accepts one regular file at a time");
    }
    if meta.len() == 0 && coding.is_some() {
        bail!("Reed-Solomon coding for an empty file is not useful; use --parity-shards 0");
    }

    let snapshot = UploadSource::capture(&source, meta.len())?;
    let snapshot_path = snapshot.path();
    let source_size = snapshot.size();
    let shard_size = shard_mib
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow!("shard size overflow"))?;
    let archive_id = explicit_id.unwrap_or_else(|| make_archive_id(&source, &meta));
    let plan_path = append_suffix(&source, ".rpool.upload.json");
    let journal_path = append_suffix(&source, ".rpool.upload.state.json");

    let plan = if plan_path.exists() {
        let plan: UploadPlan = read_json(&plan_path)?;
        if plan.source_size != source_size
            || plan.shard_size != shard_size
            || plan.archive_id != archive_id
            || plan.remotes != remotes
            || plan.placement != placement
            || plan.coding != coding
        {
            bail!(
                "existing upload plan does not match this invocation: {}\n\
                 remove it only if you intentionally want to start a new layout",
                plan_path.display()
            );
        }
        eprintln!("[resume] using upload plan: {}", plan_path.display());
        plan
    } else {
        let plan = build_upload_plan(
            rclone,
            source_size,
            shard_size,
            archive_id.clone(),
            remotes.clone(),
            placement,
            coding.clone(),
        )?;
        save_json_atomic(&plan_path, &plan)?;
        plan
    };

    if let Some(coding) = &plan.coding {
        warn_plan_failure_domains(&plan, coding);
    }

    let mut upload_journal = load_or_create_upload_journal(&journal_path, &plan)?;
    let invalidated = validate_upload_journal(storage, &snapshot_path, &plan, &mut upload_journal)?;
    if invalidated > 0 {
        eprintln!("[resume] invalidated {invalidated} stale upload journal entries");
        save_json_atomic(&journal_path, &upload_journal)?;
    }
    let completed_indexes: BTreeSet<u32> = upload_journal.completed.keys().copied().collect();
    let data_plan: Vec<PlanShard> = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Data && !completed_indexes.contains(&s.index))
        .cloned()
        .collect();

    let parity_count = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Parity)
        .count();

    eprintln!(
        "[put] {} bytes -> {} data + {} parity shards, {} workers",
        source_size,
        data_plan.len(),
        parity_count,
        workers
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;

    let shared_journal = Arc::new(Mutex::new(upload_journal));
    let data_results: Vec<Result<Shard>> = pool.install(|| {
        data_plan
            .par_iter()
            .map(|p| {
                let shard = upload_one_data_shard(storage, &snapshot_path, p, retries)?;
                record_upload_shard(&journal_path, &shared_journal, shard.clone())?;
                Ok(shard)
            })
            .collect()
    });

    for result in data_results {
        result?;
    }

    let mut shards: Vec<Shard> = shared_journal
        .lock()
        .expect("upload journal poisoned")
        .completed
        .values()
        .filter(|shard| shard.kind == ShardKind::Data)
        .cloned()
        .collect();

    if let Some(coding) = &plan.coding {
        let mut parity =
            upload_parity_groups(storage, &snapshot_path, &plan, coding, workers, retries)?;
        for shard in &parity {
            record_upload_shard(&journal_path, &shared_journal, shard.clone())?;
        }
        shards.append(&mut parity);
    }

    shards.sort_by_key(|s| s.index);

    let manifest = Manifest {
        version: 2,
        archive_id: archive_id.clone(),
        original_name: source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string()),
        original_size: source_size,
        shard_size,
        created_unix: now_unix(),
        content_root_blake3: content_root_v2(source_size, shard_size, &coding, &shards),
        coding: coding.clone(),
        shards,
    };

    validate_manifest(&manifest)?;

    let local_manifest = append_suffix(&source, ".rpool.json");
    save_json_atomic(&local_manifest, &manifest)?;
    for dst in replicate_manifest_with_storage(storage, &manifest, &remotes, retries)? {
        eprintln!("[manifest] {dst}");
    }

    if plan_path.exists() {
        fs::remove_file(&plan_path).ok();
    }
    if journal_path.exists() {
        fs::remove_file(&journal_path).ok();
    }

    println!("manifest={}", local_manifest.display());
    println!("archive_id={archive_id}");
    if let Some(pool_name) = pool_name {
        println!("pool={pool_name}");
    }
    if let Some(coding) = coding {
        println!(
            "erasure_coding={}+{} algorithm={}",
            coding.data_shards, coding.parity_shards, coding.algorithm
        );
    } else {
        println!("erasure_coding=disabled");
    }
    Ok(())
}
