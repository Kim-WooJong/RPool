use crate::erasure::{generate_parity_group, validate_rs_counts};
use crate::journal::{load_or_create_upload_journal, record_upload_shard, validate_upload_journal};
use crate::manifest::{content_root_v2, replicate_manifest_with_storage, validate_manifest};
use crate::planning::shard_from_plan;
use crate::planning::{build_upload_plan, warn_plan_failure_domains};
use crate::prelude::*;
use crate::storage::scheduler;
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
    native_crypt: bool,
) -> Result<()> {
    let original = source.canonicalize()?;
    put_with_storage(
        &StorageWriter::for_pool(rclone, native_crypt),
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
    let shard_size = crate::models::shard_size::shard_bytes(shard_mib)
        .context("invalid --shard-mib")?
        .get();
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
    let archive_id = explicit_id.unwrap_or_else(|| make_archive_id(&source, &meta));
    let plan_path = append_suffix(&source, ".rpool.upload.json");
    let journal_path = append_suffix(&source, ".rpool.upload.state.json");

    // The configured size is the maximum; small coded files use smaller
    // shards. A plan saved before that change keeps its full-size shards.
    let full_shard = shard_size;
    let shard_size = crate::models::shard_size::shard_size_for(
        source_size,
        full_shard,
        data_shards,
        parity_shards,
    );
    let plan = if plan_path.exists() {
        let plan: UploadPlan = read_json(&plan_path)?;
        if plan.source_size != source_size
            || (plan.shard_size != shard_size && plan.shard_size != full_shard)
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
        // Config wrappers may have changed since the plan was saved. Never
        // silently weaken a resumed strict layout or redistribute uploaded data.
        crate::placement::validate_resilient_plan(rclone, &plan)?;
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
    let shard_size = plan.shard_size;

    if let Some(coding) = &plan.coding {
        warn_plan_failure_domains(&plan, coding);
    }

    // No journal yet: nothing of this archive was written by an earlier try.
    let fresh = !journal_path.exists();
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

    let shared_journal = Arc::new(Mutex::new(upload_journal));
    let mut jobs: Vec<UploadJob> = data_plan.into_iter().map(UploadJob::Data).collect();
    let group_count = plan
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Parity)
        .map(|s| s.group + 1)
        .max()
        .unwrap_or(0);
    let mut next_group = 0;
    let mut live_groups = 0;
    let mut outstanding = BTreeMap::<u32, usize>::new();
    if group_count > 0 {
        jobs.push(UploadJob::Encode(next_group));
        next_group += 1;
        live_groups += 1;
    }
    // Concurrent virtual-drive uploads share one shard-transfer budget
    // (`transfer_budget`); a plain `put` has none and is unchanged.
    let budget = crate::storage::transfer_budget::current();
    let slot = || budget.as_deref().map(|b| b.acquire());
    storage.begin_upload_session(fresh);
    let run = scheduler::run(
        jobs,
        workers,
        retries,
        |job| match job {
            UploadJob::Data(s) => scheduler::remote_key(&s.remote),
            UploadJob::Parity { item, .. } => scheduler::remote_key(&item.plan.remote),
            UploadJob::Encode(_) => "\0parity-encoder".into(),
        },
        |job| match job {
            UploadJob::Data(p) => {
                let _slot = slot();
                Ok(UploadResult::Stored(upload_one_data_shard(
                    storage,
                    &snapshot_path,
                    p,
                    1,
                )?))
            }
            UploadJob::Parity { item, .. } => {
                let _slot = slot();
                let shard = shard_from_plan(&item.plan, item.blake3.clone());
                storage.write_file(&item.path, 0, &shard, 1)?;
                Ok(UploadResult::Stored(shard))
            }
            UploadJob::Encode(group) => {
                let owner = Arc::new(tempfile::tempdir()?);
                let items = generate_parity_group(
                    &snapshot_path,
                    &plan,
                    coding.as_ref().expect("parity coding"),
                    *group,
                    owner.path(),
                )?;
                Ok(UploadResult::Encoded(owner, items))
            }
        },
        crate::storage::writer::upload_retry,
        |job, result| {
            let result = result?;
            let mut more = Vec::new();
            match result {
                UploadResult::Stored(shard) => {
                    record_upload_shard(&journal_path, &shared_journal, shard)?;
                    if let UploadJob::Parity { item, .. } = &job {
                        let left = outstanding
                            .get_mut(&item.plan.group)
                            .expect("encoded group");
                        *left -= 1;
                        if *left == 0 {
                            live_groups -= 1;
                        }
                    }
                }
                UploadResult::Encoded(owner, items) => {
                    let group = match job {
                        UploadJob::Encode(g) => g,
                        _ => unreachable!(),
                    };
                    outstanding.insert(group, items.len());
                    more.extend(items.into_iter().map(|item| UploadJob::Parity {
                        item,
                        _owner: owner.clone(),
                    }));
                }
            }
            // At most two parity groups occupy staging space, independent of
            // archive length; encoding and verified transfers share one budget.
            if live_groups < 2 && next_group < group_count {
                more.push(UploadJob::Encode(next_group));
                next_group += 1;
                live_groups += 1;
            }
            Ok(more)
        },
    );
    // Provider hashes of the whole archive, one listing per account.
    let unproven = storage.finish_upload_session();
    run?;
    if !unproven.is_empty() {
        // Their journal entries would let a retry skip them: drop those so
        // the next attempt uploads them again.
        let mut journal = shared_journal.lock().expect("upload journal poisoned");
        for shard in &unproven {
            journal.completed.remove(&shard.index);
        }
        save_json_atomic(&journal_path, &*journal)?;
        bail!(
            "{} uploaded shard(s) could not be verified ({}); they will be uploaded again",
            unproven.len(),
            unproven
                .iter()
                .map(|s| s.object.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut shards: Vec<Shard> = shared_journal
        .lock()
        .expect("upload journal poisoned")
        .completed
        .values()
        .cloned()
        .collect();

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
    let publication = crate::manifest::publication_remotes(&manifest, &remotes, placement);
    for dst in replicate_manifest_with_storage(storage, &manifest, &publication, retries)? {
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

// TempDir ownership follows queued/in-flight parity jobs, including error paths.
enum UploadJob {
    Data(PlanShard),
    Encode(u32),
    Parity {
        item: GeneratedParity,
        _owner: Arc<tempfile::TempDir>,
    },
}
enum UploadResult {
    Stored(Shard),
    Encoded(Arc<tempfile::TempDir>, Vec<GeneratedParity>),
}
