pub(crate) mod adapter;
mod workspace;

use anyhow::{Context, Result};
use std::time::{Duration, Instant};

pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    if args.stop_file.as_ref().is_some_and(|path| path.exists()) {
        anyhow::bail!("stop file already exists; choose a fresh control path");
    }
    println!("Preparing durable local workspace. Imported files are restored in full; local disk space is required.");
    let mut workspace =
        workspace::Workspace::open(rclone, &args.pool, &args.workspace, args.manifests)?;
    println!(
        "Workspace pool={} targets={} (policy frozen at creation)",
        workspace.pool_name(),
        workspace.policy().remotes.len()
    );
    if args.sync_only {
        let report = workspace.sync_once()?;
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
    let cache_dir = workspace
        .files_dir()
        .parent()
        .context("workspace parent missing")?
        .join("vfs-cache");
    let mut mount = adapter::MountProcess::start(adapter::MountConfig {
        rclone: rclone.into(),
        files_dir: workspace.files_dir().to_owned(),
        cache_dir,
        target,
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
            anyhow::bail!("mount did not become ready within 30 seconds; check rclone/WinFsp/FUSE installation and mount logs. Workspace and cache retained");
        }
        if ready_reported && last_scan.elapsed() >= Duration::from_secs(args.interval_seconds) {
            match workspace.sync_once() {
                Ok(report) => {
                    println!("writeback uploaded={} logically_deleted={} unchanged={} pending={}; open/cached writes may still be pending", report.uploaded, report.deleted, report.unchanged, report.pending);
                    for warning in report.warnings {
                        eprintln!("{warning}");
                    }
                }
                Err(error) => eprintln!(
                    "Writeback incomplete; local edits retained, next scan will retry: {error:#}"
                ),
            }
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
