use crate::manifest::{
    coding_group_count, data_shards, load_manifest_with_storage, validate_manifest,
};
use crate::planning::manifest_single_provider_failure_safety;
use crate::prelude::*;
use crate::presentation::{format_bytes, print_usage_table};
use crate::storage::admin::{collect_quota_reports, BackendAdmin, RcloneAdmin};
use crate::storage::reader::StorageReader;
use crate::utils::ensure_positive;

pub(crate) fn status(
    rclone: &str,
    manifest_src: &str,
    workers: usize,
    show_usage: bool,
) -> Result<()> {
    status_with_storage(
        &StorageReader::rclone(rclone),
        rclone,
        manifest_src,
        workers,
        show_usage,
    )
}

pub(crate) fn status_with_storage(
    reader: &StorageReader,
    admin_rclone: &str,
    manifest_src: &str,
    workers: usize,
    show_usage: bool,
) -> Result<()> {
    let admin = RcloneAdmin::inherited(admin_rclone);
    status_with_services(
        reader,
        if show_usage { Some(&admin) } else { None },
        manifest_src,
        workers,
    )
}
pub(crate) fn status_with_services(
    reader: &StorageReader,
    admin: Option<&dyn BackendAdmin>,
    manifest_src: &str,
    workers: usize,
) -> Result<()> {
    ensure_positive(workers, "workers")?;
    let manifest = load_manifest_with_storage(reader, manifest_src)?;
    validate_manifest(&manifest)?;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;

    let probes: Vec<(Shard, Probe)> = pool.install(|| {
        manifest
            .shards
            .par_iter()
            .map(|shard| {
                let p = reader.probe(shard, false);
                (shard.clone(), p)
            })
            .collect()
    });

    #[derive(Default)]
    struct Counts {
        total: usize,
        ok: usize,
        missing: usize,
        bad_size: usize,
        corrupt: usize,
        errors: usize,
        data: usize,
        parity: usize,
    }

    let mut by_remote: BTreeMap<String, Counts> = BTreeMap::new();
    for (shard, probe) in &probes {
        let c = by_remote.entry(shard.remote.clone()).or_default();
        c.total += 1;
        match shard.kind {
            ShardKind::Data => c.data += 1,
            ShardKind::Parity => c.parity += 1,
        }
        match probe {
            Probe::Ok => c.ok += 1,
            Probe::Missing => c.missing += 1,
            Probe::BadSize { found, expected } => {
                let _ = (found, expected);
                c.bad_size += 1
            }
            Probe::Corrupt { found, expected } => {
                let _ = (found, expected);
                c.corrupt += 1
            }
            Probe::Error(message) => {
                let _ = message;
                c.errors += 1
            }
        }
    }

    println!("archive_id={}", manifest.archive_id);
    println!(
        "original_size={} ({})",
        manifest.original_size,
        format_bytes(manifest.original_size)
    );
    println!(
        "physical_shards={} data={} parity={}",
        manifest.shards.len(),
        data_shards(&manifest).len(),
        manifest
            .shards
            .iter()
            .filter(|s| s.kind == ShardKind::Parity)
            .count()
    );

    if let Some(coding) = &manifest.coding {
        let data_count = data_shards(&manifest).len();
        let groups = coding_group_count(data_count, coding.data_shards);
        let mut recoverable = 0usize;
        let mut degraded = 0usize;
        let mut unrecoverable = 0usize;
        let mut provider_errors = 0usize;

        for group in 0..groups {
            if probes.iter().any(|(shard, probe)| {
                shard.group == group as u32 && matches!(probe, Probe::Error(_))
            }) {
                provider_errors += 1;
                continue;
            }
            let real_data = data_shards(&manifest)
                .iter()
                .filter(|s| s.group == group as u32)
                .count();
            let virtual_zero = coding.data_shards.saturating_sub(real_data);
            let available_physical = probes
                .iter()
                .filter(|(s, p)| s.group == group as u32 && p.is_ok())
                .count();
            let unavailable_physical = probes
                .iter()
                .filter(|(s, p)| s.group == group as u32 && !p.is_ok())
                .count();

            if available_physical + virtual_zero >= coding.data_shards {
                recoverable += 1;
                if unavailable_physical > 0 {
                    degraded += 1;
                }
            } else {
                unrecoverable += 1;
            }
        }

        let overhead = coding.parity_shards as f64 / coding.data_shards as f64 * 100.0;
        println!(
            "coding={} data={} parity={} overhead={:.1}% groups={}",
            coding.algorithm, coding.data_shards, coding.parity_shards, overhead, groups
        );
        println!(
            "recoverable_groups={}/{} degraded_groups={} unrecoverable_groups={} provider_error_groups={}",
            recoverable, groups, degraded, unrecoverable, provider_errors
        );
        let (single_provider_safe, max_provider_shards) =
            manifest_single_provider_failure_safety(&manifest, coding);
        println!(
            "single_provider_failure_safe={} known_min_max_group_shards_on_one_configured_remote={} parity_budget={}",
            single_provider_safe, max_provider_shards, coding.parity_shards
        );
    } else {
        println!("coding=disabled");
    }

    for (remote, c) in &by_remote {
        println!(
            "remote={} total={} data={} parity={} ok={} missing={} bad_size={} corrupt={} errors={}",
            remote, c.total, c.data, c.parity, c.ok, c.missing, c.bad_size, c.corrupt, c.errors
        );
    }

    if let Some(admin) = admin {
        println!();
        let remotes: Vec<String> = by_remote.keys().cloned().collect();
        let capacity_remotes = admin.catalog()?.capacity_remotes(&remotes)?;
        print_usage_table(&collect_quota_reports(admin, &capacity_remotes, workers)?);
    }

    Ok(())
}
