//! The `rpool mount --virtual-drive` entry point and its network monitor.

use super::*;

pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    let workspace_started = std::time::Instant::now();
    if args.recover_spool {
        let paths = recover_spool(&args.workspace)?;
        println!("Exported {} local writes under recovered-writes; partial uploads are explicitly labelled. Checkpoints, spool and remote history unchanged.", paths.len());
        return Ok(());
    }
    if args.migrate_excluded {
        bail!("Active archive migration currently uses replica workspaces; virtual history is retained");
    }
    if args.stop_file.as_ref().is_some_and(|p| p.exists()) {
        bail!("stop file already exists");
    }
    let frontend = args.frontend.resolve(&args);
    if args.frontend != crate::cli::Frontend::Auto
        && frontend != crate::cli::Frontend::Dav
        && (args.bounded_shared || args.shared_root.is_some() || !args.virtual_drive)
    {
        bail!("native frontends serve online drives in local or pool-sync mode; use --frontend dav for bounded shared or shared-root workspaces");
    }
    if frontend != crate::cli::Frontend::Dav {
        // Only an explicit choice gets here unavailable; `auto` already fell back.
        frontend.ensure_available()?;
    }
    if args.native_read_only && frontend == crate::cli::Frontend::Dav {
        bail!("--native-read-only requires a native frontend");
    }
    let stop = crate::mount::lifecycle::StopControl::new(args.stop_file.clone())?;
    let generated_worker;
    let worker = if args.pool_sync {
        if let Some(worker) = &args.pool_worker {
            worker.as_str()
        } else {
            let saved = args.workspace.join("pool-worker.json");
            generated_worker = if saved.exists() {
                crate::utils::read_json::<String>(&saved)?
            } else {
                format!("pc-{}", &random_id()?[..12])
            };
            &generated_worker
        }
    } else {
        args.worker_name.as_deref().unwrap_or("local")
    };
    println!("Opening virtual workspace");
    // A pool migration may have adopted the drive into a new generation.
    let adopted = if args.pool_sync {
        crate::mount::adoption_fence::before_open(
            rclone,
            &args.pool,
            &args.workspace,
            args.pool_retention,
        )?
    } else {
        None
    };
    let cache_limit = args
        .cache_gib
        .checked_mul(1073741824)
        .context("cache limit overflow")?;
    let mut drive = match adopted {
        Some(epoch) => VirtualDrive::open_with_epoch(
            rclone,
            &args.pool,
            &args.workspace,
            worker,
            args.shared_root.as_deref(),
            cache_limit,
            args.bounded_shared,
            args.pool_sync,
            args.pool_retention,
            &epoch,
        )?,
        None => VirtualDrive::open(
            rclone,
            &args.pool,
            &args.workspace,
            worker,
            args.shared_root.as_deref(),
            cache_limit,
            args.bounded_shared,
            args.pool_sync,
            args.pool_retention,
        )?,
    };
    if args.pool_sync {
        drive.pool_history_limit = crate::mount::pool_sync::Config::load(
            &drive.root,
            args.pool_history_limit.map(|v| v as usize),
        )?
        .history_limit;
        durable_json(&drive.root.join("pool-worker.json"), &worker)?;
    }
    drive.checkpoint_coordinator = args.shared_coordinator;
    drive.checkpoint_keep = args.shared_keep_previous;
    drive.spool_limit = args
        .spool_gib
        .checked_mul(1073741824)
        .context("spool limit overflow")?;
    let drive = Arc::new(drive);
    println!(
        "Virtual workspace opened in {:.3}s",
        workspace_started.elapsed().as_secs_f64()
    );
    if let Some(deferral) = &drive.layout_deferral {
        println!("{}", deferral.message());
    }
    if let Some(path) = &args.status_file {
        let destination = path.with_file_name(crate::mount::layout_refresh::STATUS_FILE);
        let result = match &drive.layout_deferral {
            Some(deferral) => durable_json(&destination, deferral),
            None => match fs::remove_file(&destination) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        };
        if let Err(error) = result {
            eprintln!("Layout status unavailable: {error:#}");
        }
    }
    if args.apply_retention {
        let report = drive.apply_retention(args.keep_previous, args.exclusive_archive_ownership)?;
        println!("Retention completed: {} obsolete tracked archives, {} exact objects removed ({} logical data bytes; backend trash/versioning may delay quota recovery).", report.obsolete_archives, report.objects.len(), report.reclaimable_bytes);
        return Ok(());
    }
    if drive.root.join("retention-journal.json").exists() {
        bail!("Interrupted retention: resume --apply-retention --exclusive-archive-ownership before mounting or syncing. Do not remove the journal.");
    }
    if args.retention_report {
        println!(
            "{}",
            serde_json::to_string_pretty(&drive.retention_report(args.keep_previous)?)?
        );
        return Ok(());
    }
    for source in &args.manifests {
        drive.import(source)?;
    }
    if args.cleanup_cache {
        println!(
            "clean_cache_bytes_removed={} committed_spool_bytes_removed={}; dirty/unknown spool and remote history retained",
            drive.cache.startup_removed_bytes.saturating_add(drive.cache.cleanup()?), drive.cleanup_committed_spool()?
        );
        return Ok(());
    }
    println!("Synchronizing cloud metadata before mount; account failures retain local data");
    let cloud_started = std::time::Instant::now();
    if args.capacity_only || args.import_from.is_some() {
        drive.pull()?;
    } else if args.sync_only {
        drive.sync()?;
    } else if drive.bounded_shared && drive.checkpoint_coordinator {
        // Coordinator sync also initializes a new shared root. Retain that
        // bootstrap until a separate metadata-only initialization exists.
        drive.sync().context("Cloud synchronization required before opening this shared drive; local work is retained")?;
    } else if drive.bounded_shared || !drive.pool_sync_roots.is_empty() {
        // Refresh the authoritative namespace, but do not delay mounting for
        // pending uploads, private copies, replication or remote collection.
        // Failed metadata discovery remains fatal; durable local work is retained.
        drive.pull().context(
            "Cloud metadata required before opening this shared drive; local work is retained",
        )?;
    } else if let Err(e) = drive.pull() {
        eprintln!("Virtual metadata refresh pending, durable local state retained: {e:#}");
    }
    println!(
        "Cloud pre-mount phase completed in {:.3}s",
        cloud_started.elapsed().as_secs_f64()
    );
    let report_drive = drive.clone();
    let status_file = args.status_file.clone();
    let report = Arc::new(move || {
        let drive = &report_drive;
        if !drive.pool_sync_roots.is_empty() {
            if let Some(path) = &status_file {
                let destination = path.with_file_name("pool-sync-status.json");
                let result = drive
                    .pool_status()
                    .and_then(|status| durable_json(&destination, &status));
                if let Err(error) = result {
                    let _ = fs::remove_file(&destination);
                    eprintln!("Pool sync status unavailable: {error:#}");
                }
            }
        }
        match drive.refresh_capacity() {
            Ok(status) => {
                println!(
                    "Virtual namespace: logical_used={} additional_estimate={} pending={}",
                    status.logical_used,
                    status.additional_estimate,
                    drive.state.lock().unwrap().pending.len()
                );
                if let Some(path) = &status_file {
                    if let Err(e) = durable_json(path, &status) {
                        eprintln!("Status snapshot failed: {e:#}");
                    }
                }
            }
            Err(e) => {
                if let Some(path) = &status_file {
                    let _ = fs::remove_file(path);
                }
                eprintln!("Quota status unavailable: {e:#}");
            }
        }
    });
    if args.import_from.is_some() {
        crate::mount::rclone_import::run(&drive, &args, &|| stop.requested())?;
        report();
        return Ok(());
    }
    if args.capacity_only || args.sync_only {
        report();
        return Ok(());
    }
    println!("Capacity verification and pending uploads will run after mount readiness");
    if stop.requested() {
        println!("Mount cancelled before native startup; local data retained");
        return Ok(());
    }
    // Lives until this function returns: registry entry and live status.
    let _monitor = net_monitor(&drive, &args, frontend);
    if drive.bounded_shared {
        drive.isolate_previous_native_cache()?;
    } else if frontend != crate::cli::Frontend::Dav || !drive.pool_sync_roots.is_empty() {
        // rclone may not replay its cache here (native frontend, or peers may
        // have written since), so RPool imports the unsaved writes itself.
        drive.recover_previous_cache()?;
    }
    if frontend != crate::cli::Frontend::Dav {
        return crate::mount::frontend::run_native(
            drive,
            crate::mount::frontend::NativeRun {
                frontend,
                mountpoint: args.mountpoint.as_deref().context("mountpoint required")?,
                read_only: args.native_read_only,
                interval: std::time::Duration::from_secs(args.interval_seconds),
                stop: &stop,
                report,
            },
        );
    }
    println!("Starting virtual filesystem and native mount");
    let server = crate::mount::dav::Server::start(drive.clone(), args.diagnostic_read_only)?;
    let mut mount =
        crate::mount::adapter::MountProcess::start(crate::mount::adapter::MountConfig {
            rclone: rclone.into(),
            files_dir: drive.root.join("anchor"),
            cache_dir: drive.root.join("vfs-cache"),
            vfs_cache_gib: args.vfs_cache_gib,
            cache_min_free_gib: args.cache_min_free_gib,
            target: args.mountpoint.context("mountpoint required")?,
            shared: true,
            read_only: args.diagnostic_read_only,
            webdav: Some((format!("http://{}/", server.address), server.token.clone())),
            volume_name: Some(args.pool.clone()),
        })?;
    if !drive.pool_sync_roots.is_empty() {
        println!("Pool sync ready: metadata inside {} pool roots; no coordinator. Previous versions={}, automatic private-snapshot collection={}. Legacy v6 data is untouched.", drive.pool_sync_roots.len(), drive.pool_history_limit, drive.peer_retention);
    } else if drive.bounded_shared {
        println!("Shared drive starting: cloud-authoritative namespace synchronized; file bytes download on demand. Local writes await coordinator acceptance; no distributed locking guarantee.");
    } else {
        println!("Virtual drive starting: metadata-only listing, verified shard reads, durable local write spool. Incoming updates to served paths appear as incoming revision copies until remount; no distributed locking guarantee.");
    }
    let start = std::time::Instant::now();
    let mut ready = false;
    let mut maintenance = crate::mount::maintenance::Maintenance::new(
        std::time::Duration::from_secs(args.interval_seconds),
    );
    let report: Arc<dyn Fn() + Send + Sync> = report;
    let outcome: Result<()> = (|| {
        loop {
            for line in mount.logs() {
                eprintln!("{line}");
            }
            if let Some(code) = mount.poll()? {
                bail!("virtual mount exited {code}; spool and cache retained");
            }
            if stop.requested() {
                break;
            }
            if !ready && mount.ready() {
                ready = true;
                println!(
                    "Virtual filesystem ready after {:.3}s native startup. Native saves may remain in rclone VFS cache before reaching RPool spool; cloud replication is asynchronous.",
                    start.elapsed().as_secs_f64()
                );
            }
            if !ready && start.elapsed() > std::time::Duration::from_secs(30) {
                bail!("virtual mount readiness timeout; state retained");
            }
            if !args.diagnostic_read_only && ready {
                maintenance.poll(&drive, &report, &stop.flag)?;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Ok(())
    })();
    stop.cancel();
    println!(
        "Stopping native mount and cancelling remote work; pending local data will be retained"
    );
    let stopped = mount.stop();
    if stopped.is_err() && mount.is_running() {
        // Dropping `server` now would leave a live rclone NFS/FUSE server without its
        // backend while the kernel may still send it I/O. Keep serving until it exits.
        eprintln!("Native mount process is still running after the stop request; keeping the RPool WebDAV backend alive until it exits so kernel I/O can finish. Do not force-quit RPool.");
        let waiting = std::time::Instant::now();
        loop {
            for line in mount.logs() {
                eprintln!("{line}");
            }
            match mount.wait_for_exit(std::time::Duration::from_secs(30)) {
                Ok(true) => {
                    println!(
                        "Native mount process exited after {}s of extra waiting",
                        waiting.elapsed().as_secs()
                    );
                    break;
                }
                Ok(false) => eprintln!(
                    "Still waiting for the native mount process ({}s); WebDAV backend retained",
                    waiting.elapsed().as_secs()
                ),
                Err(error) => {
                    eprintln!("Native mount process state after exit: {error:#}");
                    break;
                }
            }
        }
    }
    let joined = maintenance.join();
    let stopped = stopped?;
    println!(
        "Virtual mount stopped; forced={} pending spool/cache/history retained. Cloud replication was not drained; use sync-only separately or resume this workspace.",
        stopped.forced
    );
    drop(server);
    joined?;
    outcome
}

/// Network monitor of a mounted virtual drive; its queue is the drive's
/// pending intents.
pub(super) fn net_monitor(
    drive: &Arc<VirtualDrive>,
    args: &crate::cli::MountArgs,
    frontend: crate::cli::Frontend,
) -> crate::monitor::runtime::MountMonitor {
    use crate::monitor::{runtime, sampler::PendingItem};
    let queue_drive = drive.clone();
    let queue: runtime::QueueFn = Box::new(move || {
        let Ok(state) = queue_drive.state.lock() else {
            return Vec::new();
        };
        state
            .pending
            .iter()
            .map(|intent| PendingItem {
                id: intent.id.clone(),
                size: intent.size,
                spool: intent
                    .spool
                    .is_some()
                    .then(|| queue_drive.spool_path(intent)),
            })
            .collect()
    });
    runtime::MountMonitor::start(
        runtime::Spec {
            pool: drive.pool.clone(),
            workspace: args.workspace.clone(),
            mountpoint: args
                .mountpoint
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            frontend: frontend.cli_value().into(),
            remotes: drive.policy.remotes.clone(),
            rclone: drive.rclone.clone(),
        },
        queue,
    )
}
