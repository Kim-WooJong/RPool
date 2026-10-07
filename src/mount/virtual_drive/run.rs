//! The `rpool mount` entry point and its network monitor.

use super::*;

/// `rpool mount` for a virtual drive: handles `--recover-spool`, picks the
/// frontend and worker name, opens (or adopts) the workspace and runs the
/// mount or its offline action. Called by `mount::run`.
pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    let workspace_started = std::time::Instant::now();
    if args.recover_spool {
        let paths = recover_spool(&args.workspace)?;
        println!("Exported {} local writes under recovered-writes; partial uploads are explicitly labelled. Checkpoints, spool and remote history unchanged.", paths.len());
        return Ok(());
    }
    if args.stop_file.as_ref().is_some_and(|p| p.exists()) {
        bail!("stop file already exists");
    }
    let frontend = args.frontend.resolve();
    if frontend != crate::cli::Frontend::Dav {
        // Only an explicit choice gets here unavailable; `auto` already fell back.
        frontend.ensure_available()?;
    }
    if args.native_read_only && frontend == crate::cli::Frontend::Dav {
        bail!("--native-read-only requires a native frontend");
    }
    // Shutdown signals stop the mount like the stop file: clean unmount, no
    // rclone left behind. The guard tells a Windows close event when done.
    let _signals = crate::mount::shutdown_signal::install();
    let stop = crate::mount::lifecycle::StopControl::new(args.stop_file.clone())?;
    let generated_worker;
    let worker = if let Some(worker) = &args.pool_worker {
        worker.as_str()
    } else {
        let saved = args.workspace.join("pool-worker.json");
        generated_worker = if saved.exists() {
            crate::utils::read_json::<String>(&saved)?
        } else {
            format!("pc-{}", &random_id()?[..12])
        };
        &generated_worker
    };
    println!("Opening virtual workspace");
    // A pool migration may have adopted the drive into a new generation.
    let adopted = crate::mount::adoption_fence::before_open(rclone, &args.pool, &args.workspace)?;
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
            cache_limit,
            &epoch,
        )?,
        None => {
            VirtualDrive::open_for_mount(rclone, &args.pool, &args.workspace, worker, cache_limit)?
        }
    };
    durable_json(&drive.root.join("pool-worker.json"), &worker)?;
    if let Some(workers) = args.workers {
        // An execution knob of this PC; the pool keeps its layout.
        drive.policy.workers = usize::try_from(workers).unwrap_or(usize::MAX);
    }
    drive.spool_limit = args
        .spool_gib
        .checked_mul(1073741824)
        .context("spool limit overflow")?;
    // Also used by `--sync-only` and import batches.
    let files = match args.upload_files {
        Some(files) => usize::try_from(files).unwrap_or(usize::MAX),
        None => super::upload_wake::auto_upload_files(
            drive.policy.workers,
            drive.policy.data_shards + drive.policy.parity_shards,
        ),
    };
    drive.upload.set_files(files);
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
    } else {
        // Refresh the authoritative namespace, but do not delay mounting for
        // pending uploads, replication or remote collection. Failed metadata
        // discovery remains fatal; durable local work is retained.
        drive.pull().context(
            "Cloud metadata required before opening this shared drive; local work is retained",
        )?;
    }
    println!(
        "Cloud pre-mount phase completed in {:.3}s",
        cloud_started.elapsed().as_secs_f64()
    );
    let report_drive = drive.clone();
    let status_file = args.status_file.clone();
    let report = Arc::new(move || {
        let drive = &report_drive;
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
    let mut monitor = net_monitor(&drive, &args, frontend);
    // rclone may not replay its cache here (native frontend, or peers may
    // have written since), so RPool imports the unsaved writes itself.
    drive.recover_previous_cache()?;
    if frontend != crate::cli::Frontend::Dav {
        let outcome = crate::mount::frontend::run_native(
            drive.clone(),
            crate::mount::frontend::NativeRun {
                frontend,
                mountpoint: args.mountpoint.as_deref().context("mountpoint required")?,
                volume_name: &args.pool,
                read_only: args.native_read_only,
                interval: std::time::Duration::from_secs(args.interval_seconds),
                stop: &stop,
                report: report.clone(),
            },
        );
        match outcome {
            Err(e) if auto_falls_back_to_dav(&args, &e) => {
                eprintln!("{e:#}\nFalling back to WebDAV for this mount (`--frontend auto`).");
                drop(monitor);
                monitor = net_monitor(&drive, &args, crate::cli::Frontend::Dav);
            }
            other => return other,
        }
    }
    let _monitor = monitor;
    println!("Starting virtual filesystem and native mount");
    let server = crate::mount::dav::Server::start(drive.clone())?;
    let mut mount =
        crate::mount::adapter::MountProcess::start(crate::mount::adapter::MountConfig {
            rclone: rclone.into(),
            anchor_dir: drive.root.join("anchor"),
            cache_dir: drive.root.join("vfs-cache"),
            vfs_cache_gib: args.vfs_cache_gib,
            cache_min_free_gib: args.cache_min_free_gib,
            target: args.mountpoint.context("mountpoint required")?,
            webdav: (format!("http://{}/", server.address), server.token.clone()),
            volume_name: Some(args.pool.clone()),
        })?;
    println!("Pool sync ready: metadata inside {} pool roots; no coordinator. File bytes download on demand; no distributed locking guarantee.", drive.pool_sync_roots.len());
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
            if ready {
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
    let joined = maintenance.join(&drive);
    // A slow but clean macOS unmount (finished during the extra wait) is a
    // normal stop, not the grace-period error recorded before it.
    let stopped = match stopped {
        Err(_) if mount.exited_cleanly() => Ok(crate::mount::adapter::StopReport { forced: false }),
        other => other,
    }?;
    println!(
        "Virtual mount stopped; forced={} pending spool/cache/history retained. Cloud replication was not drained; use sync-only separately or resume this workspace.",
        stopped.forced
    );
    drop(server);
    joined?;
    outcome
}

/// Whether a failed native start may continue with WebDAV: only on macOS,
/// where macFUSE can be installed but not yet approved (RPool cannot probe
/// that), only for `--frontend auto`, never for a read-only request (WebDAV
/// would mount writable), and only when nothing was mounted yet.
fn auto_falls_back_to_dav(args: &crate::cli::MountArgs, error: &anyhow::Error) -> bool {
    cfg!(target_os = "macos")
        && args.frontend == crate::cli::Frontend::Auto
        && !args.native_read_only
        && crate::mount::frontend::native_start_failed(error)
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
