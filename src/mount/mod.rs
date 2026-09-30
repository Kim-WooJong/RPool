mod account_recovery;
pub(crate) mod adapter;
pub(crate) mod cache_recovery;
pub(crate) mod capacity;
mod crash;
mod dav;
mod frontend;
mod fs_core;
mod incremental;
mod lifecycle;
mod maintenance;
mod namespace;
mod native_ancestry;
pub(crate) mod peer_projection;
mod peer_snapshot;
mod peer_snapshot_model;
mod peer_snapshot_transport;
pub(crate) mod pool_sync;
mod pool_transition;
pub(crate) mod rclone_import;
mod retention;
mod shard_cache;
mod shared_checkpoint;
mod shared_checkpoint_model;
mod shared_checkpoint_transport;
mod shared_model;
/// Synthetic metadata for read-only listing tests outside `mount`.
#[cfg(test)]
pub(crate) use shared_model::{Content as TestContent, Event as TestEvent};
mod shared_transport;
mod virtual_drive;
mod workspace;

use anyhow::{Context, Result};
use std::time::{Duration, Instant};

pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    if args.recovery_reprocess_plan.is_some()
        && args.account_recovery_from.is_none()
        && !args.apply_pool_changes
    {
        anyhow::bail!("Reprocess plan requires account recovery or --apply-pool-changes");
    }
    if args.apply_pool_changes {
        return pool_transition::run(rclone, &args);
    }
    let _transition_lock = pool_transition::lock_workspace_transition(&args.workspace)?;
    pool_transition::assert_no_incomplete_transition(&args.workspace)?;
    if args.account_recovery_from.is_some() {
        return account_recovery::run(rclone, &args);
    }
    crate::storage::admin::domains::update(&args.capacity_domain, &args.failure_domain)?;
    if args.virtual_drive {
        return virtual_drive::run(rclone, args);
    }
    if args.stop_file.as_ref().is_some_and(|path| path.exists()) {
        anyhow::bail!("stop file already exists; choose a fresh control path");
    }
    println!("Preparing durable local workspace. Imported files are restored in full; local disk space is required.");
    let mut workspace =
        workspace::Workspace::open(rclone, &args.pool, &args.workspace, args.manifests)?;
    workspace.configure_shared(args.shared_root.as_deref(), args.worker_name.as_deref())?;
    println!(
        "Workspace pool={} targets={} (policy frozen at creation)",
        workspace.pool_name(),
        workspace.policy().remotes.len()
    );
    report_capacity(&workspace, args.status_file.as_deref());
    if args.capacity_only {
        return Ok(());
    }
    if args.migrate_excluded {
        let moved = workspace.migrate_excluded()?;
        report_capacity(&workspace, args.status_file.as_deref());
        println!("Migration completed: {moved} active archive references switched; original remote data retained.");
        return Ok(());
    }
    if args.sync_only {
        let report = workspace.sync_once()?;
        workspace.sync_shared(true)?;
        report_capacity(&workspace, args.status_file.as_deref());
        println!(
            "writeback uploaded={} logically_deleted={} unchanged={} pending={}",
            report.uploaded, report.deleted, report.unchanged, report.pending
        );
        for warning in report.warnings {
            eprintln!("{warning}");
        }
        println!("Local scan archived. Detached VFS cache may still contain pending edits; restart the original mount to drain it.");
        return Ok(());
    }
    if args.stop_file.as_ref().is_some_and(|path| path.exists()) {
        return Ok(());
    }
    let target = args.mountpoint.context("mountpoint required")?;
    // Before mounting there can be no new VFS writes. Existing cache is checked
    // independently; pending recovery never grants permission to replace files.
    if workspace.is_shared() {
        match workspace.sync_once() {
            Ok(_) => workspace.sync_shared(true)?,
            Err(error) => eprintln!("Pre-mount writeback deferred; retaining local edits and skipping incoming replacement: {error:#}"),
        }
    }
    let cache_dir = workspace
        .files_dir()
        .parent()
        .context("workspace parent missing")?
        .join("vfs-cache");
    let mut mount = adapter::MountProcess::start(adapter::MountConfig {
        rclone: rclone.into(),
        files_dir: workspace.files_dir().to_owned(),
        cache_dir,
        vfs_cache_gib: args.vfs_cache_gib,
        cache_min_free_gib: args.cache_min_free_gib,
        target,
        shared: workspace.is_shared(),
        read_only: false,
        webdav: None,
        volume_name: Some(args.pool.clone()),
    })?;
    println!("Mount process started. Waiting for filesystem readiness; close files before requesting unmount.");
    let mut last_scan = Instant::now();
    let startup = Instant::now();
    let mut ready_reported = false;
    loop {
        for line in mount.logs() {
            eprintln!("{line}");
        }
        if let Some(status) = mount.poll()? {
            anyhow::bail!("mount process exited ({status}); local files and VFS cache retained. Restart with the same workspace and mount configuration");
        }
        if args.stop_file.as_ref().is_some_and(|path| path.exists()) {
            break;
        }
        if !ready_reported && mount.ready() {
            ready_reported = true;
            println!("Mounted read/write. Local changes are archived periodically; this is not immediate cloud durability.");
        }
        if !ready_reported && startup.elapsed() > Duration::from_secs(30) {
            let hint = if cfg!(target_os = "macos") {
                "rclone nfsmount, macOS NFS and mount logs"
            } else {
                "rclone/WinFsp/FUSE installation and mount logs"
            };
            anyhow::bail!("mount did not become ready within 30 seconds; check {hint}. Workspace and cache retained");
        }
        if ready_reported && last_scan.elapsed() >= Duration::from_secs(args.interval_seconds) {
            match workspace.sync_once() {
                Ok(report) => {
                    if let Err(error) = workspace.sync_shared(false) {
                        eprintln!("Shared sync pending; local data retained: {error:#}");
                    }
                    println!("writeback uploaded={} logically_deleted={} unchanged={} pending={}; open/cached writes may still be pending", report.uploaded, report.deleted, report.unchanged, report.pending);
                    for warning in report.warnings {
                        eprintln!("{warning}");
                    }
                }
                Err(error) => eprintln!(
                    "Writeback incomplete; local edits retained, next scan will retry: {error:#}"
                ),
            }
            report_capacity(&workspace, args.status_file.as_deref());
            last_scan = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Ok(stats) = mount.vfs_stats() {
        println!("Local VFS state before unmount (not cloud durability): {stats}");
    }
    let stopped = mount.stop()?;
    println!(
        "Unmount requested; forced={} cache_preserved={}",
        stopped.forced, stopped.cache_preserved
    );
    let report = workspace.sync_once().context(
        "unmounted, but final cloud writeback failed; local files and VFS cache retained",
    )?;
    workspace.sync_shared(!stopped.forced)?;
    println!(
        "Final local scan: uploaded={} logically_deleted={} unchanged={} pending={}",
        report.uploaded, report.deleted, report.unchanged, report.pending
    );
    for warning in report.warnings {
        eprintln!("{warning}");
    }
    println!("Workspace and VFS cache retained. Cached/open writes may require restarting this same mount; final scan is not proof that every cached write reached the pool.");
    Ok(())
}

fn report_capacity(workspace: &workspace::Workspace, path: Option<&std::path::Path>) {
    match workspace.capacity_status() {
        Ok(status) => {
            println!("Pool capacity: logical_used={} additional_estimate={} logical_ceiling_estimate={} eligible={} excluded={} affected_active={} retained_archives={}",
                status.logical_used, status.additional_estimate, status.logical_ceiling_estimate,
                status.eligible.len(), status.excluded.len(), status.affected_active, status.retained_archives);
            for excluded in &status.excluded {
                eprintln!(
                    "Excluded from new data placement: {} — {}",
                    excluded.remote, excluded.reason
                );
            }
            if let Some(path) = path {
                let result: anyhow::Result<()> = (|| {
                    let mut file = tempfile::NamedTempFile::new_in(
                        path.parent().context("status parent missing")?,
                    )?;
                    serde_json::to_writer(&mut file, &status)?;
                    file.as_file().sync_all()?;
                    file.persist(path).map_err(|e| e.error)?;
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = std::fs::remove_file(path);
                    eprintln!("Capacity status write failed: {e:#}");
                }
            }
        }
        Err(e) => {
            // Remove stale success rather than display a previous capacity as current.
            if let Some(path) = path {
                let _ = std::fs::remove_file(path);
            }
            eprintln!(
                "Capacity unavailable: {e:#}; new uploads still require successful quota checks"
            );
        }
    }
}

#[cfg(test)]
mod native_sync_tests;
#[cfg(test)]
mod virtual_tests;
