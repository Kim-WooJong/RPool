//! Where a drive-history request runs.
//!
//! 1. The pool (or the given workspace) is mounted on this PC: the request
//!    goes to that mount process (request file, `request`). Only it may open
//!    the workspace (it holds the lock), and its namespace, pins and sync
//!    see the change immediately.
//! 2. A given workspace that is not mounted: opened directly (takes the lock
//!    like a mount), changes are published as that workspace's worker.
//! 3. No workspace: listing/preview read the cloud metadata read-only (like
//!    `pool browse`); purge publishes a mark; restore/rollback run in a
//!    scratch workspace on the newest drive generation, published as this
//!    PC's worker (`rpool-<host>`). The scratch is removed once everything is
//!    published, else kept (and named) so nothing unpublished is lost.
use super::ops::Op;
use super::request::Answer;
use crate::mount::history_bridge::{self as bridge, VirtualDrive};
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Where `dispatch::run` sends a request (see the module docs).
pub(crate) enum Route {
    /// A running pool-sync mount owns this workspace: submit a request file to it.
    Mount(PathBuf),
    /// An unmounted workspace: open it directly.
    Workspace(PathBuf),
    /// No workspace: read the cloud, or use a scratch workspace for writes.
    Cloud,
}

/// Same directory, comparing canonical paths when both resolve.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `is_drive`: whether a mounted workspace is an online (pool-sync) drive
/// (other mounts run no maintenance loop and would never answer).
pub(crate) fn route(
    pool: &str,
    workspace: Option<&Path>,
    mounts: &[crate::monitor::model::MountEntry],
    is_drive: &dyn Fn(&Path) -> bool,
) -> Route {
    let mounted = |m: &&crate::monitor::model::MountEntry| is_drive(Path::new(&m.workspace));
    match workspace {
        Some(ws) => match mounts
            .iter()
            .filter(mounted)
            .find(|m| same_dir(Path::new(&m.workspace), ws))
        {
            Some(m) => Route::Mount(PathBuf::from(&m.workspace)),
            None => Route::Workspace(ws.to_path_buf()),
        },
        None => match mounts.iter().filter(mounted).find(|m| m.pool == pool) {
            Some(m) => Route::Mount(PathBuf::from(&m.workspace)),
            None => Route::Cloud,
        },
    }
}

/// Whether `path` holds a pool-sync drive binding (`virtual.json` version 6 or 7).
fn is_pool_sync_workspace(path: &Path) -> bool {
    #[derive(Deserialize)]
    struct Binding {
        version: u32,
    }
    crate::utils::read_json::<Binding>(&path.join("virtual.json"))
        .is_ok_and(|b| matches!(b.version, 6 | 7))
}

/// Runs `op` for `pool`; returns the JSON value and notes for stderr.
pub(crate) fn run(rclone: &str, pool: &str, workspace: Option<&Path>, op: &Op) -> Result<Answer> {
    crate::pool::validate_pool_name(pool)?;
    match route(
        pool,
        workspace,
        &crate::monitor::active_mounts(),
        &is_pool_sync_workspace,
    ) {
        Route::Mount(ws) => super::request::submit(&ws, pool, op),
        Route::Workspace(ws) => {
            let drive = bridge::open_workspace(rclone, pool, &ws)?;
            on_drive(&drive, rclone, pool, op)
        }
        Route::Cloud => cloud(rclone, pool, op),
    }
}

/// Runs `op` on an open drive (workspace, scratch or inside the mount).
pub(crate) fn on_drive(drive: &VirtualDrive, rclone: &str, pool: &str, op: &Op) -> Result<Answer> {
    if let Op::Cleanup(request) = op {
        let report = super::cleanup::run(rclone, pool, Some(drive), request)?;
        return Ok((serde_json::to_value(report)?, Vec::new()));
    }
    let now = crate::utils::now_unix();
    let loaded = super::load::from_drive(drive, rclone, now)?;
    let retention = super::retention::load(pool)?;
    let history = &loaded.history;
    let value = if op.writes_drive() {
        super::ops::write(op, drive, history, pool, &retention, now)?
    } else if op.marks() {
        let stores =
            super::marks::Remote::open(rclone, bridge::roots(drive), drive.policy.native_crypt)?;
        let stores: Vec<&dyn super::marks::MarkStore> = stores.iter().map(|s| s as _).collect();
        let worker = bridge::worker(drive);
        super::ops::purge(op, &stores, history, pool, &retention, &worker, now)?
    } else {
        super::ops::read(op, history, pool, &retention, now)?
    };
    Ok((value, loaded.notes))
}

/// Mount side: answers waiting requests (called by the maintenance loop).
pub(crate) fn serve_mount(drive: &VirtualDrive) -> Result<usize> {
    super::request::serve(&drive.root, &|request| {
        if request.pool != drive.pool {
            bail!(
                "this mount serves pool {}, not {}",
                drive.pool,
                request.pool
            );
        }
        on_drive(drive, &drive.rclone, &drive.pool, &request.op)
    })
}

/// Workspace-less path: cleanup, scratch-workspace writes, cloud purge marks
/// or read-only listing/preview of the cloud metadata.
fn cloud(rclone: &str, pool: &str, op: &Op) -> Result<Answer> {
    if let Op::Cleanup(request) = op {
        let report = super::cleanup::run(rclone, pool, None, request)?;
        return Ok((serde_json::to_value(report)?, Vec::new()));
    }
    if op.writes_drive() {
        return scratch(rclone, pool, op);
    }
    let now = crate::utils::now_unix();
    let loaded = super::load::from_cloud(rclone, pool, now)?;
    let retention = super::retention::load(pool)?;
    let value = if op.marks() {
        let generation = super::load::generation(rclone, pool)?.context("no online drive")?;
        let roots = super::load::generation_roots(pool, &generation)?;
        let native = super::load::pool_definition(pool)?.native_crypt;
        let stores = super::marks::Remote::open(rclone, &roots, native)?;
        let stores: Vec<&dyn super::marks::MarkStore> = stores.iter().map(|s| s as _).collect();
        super::ops::purge(
            op,
            &stores,
            &loaded.history,
            pool,
            &retention,
            &cloud_worker(),
            now,
        )?
    } else {
        super::ops::read(op, &loaded.history, pool, &retention, now)?
    };
    Ok((value, loaded.notes))
}

/// Worker label of this PC for workspace-less changes.
pub(crate) fn cloud_worker() -> String {
    let host = ["COMPUTERNAME", "HOSTNAME", "HOST"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .unwrap_or_default();
    let clean: String = host
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if clean.is_empty() {
        "rpool-pc".into()
    } else {
        format!("rpool-{clean}")
    }
}

/// Whether the drive still has local writes or events not yet published.
fn unpublished(drive: &VirtualDrive) -> Result<bool> {
    if !bridge::pending_paths(drive).is_empty() {
        return Ok(true);
    }
    Ok(!bridge::v6_events(drive).1.is_empty())
}

/// Run a writing op in a fresh scratch workspace under
/// `<config>/drive-history/scratch/<id>`; the scratch is deleted once all is
/// published, else kept and named in a note/error so it can be finished.
fn scratch(rclone: &str, pool: &str, op: &Op) -> Result<Answer> {
    let generation = super::load::generation(rclone, pool)?
        .with_context(|| format!("pool {pool} has no online drive (pool-sync metadata) yet"))?;
    let dir = crate::config::app_config_dir()?
        .join("drive-history")
        .join("scratch")
        .join(crate::monitor::registry::new_id());
    let drive = bridge::open_scratch(rclone, pool, &dir, &cloud_worker(), &generation)?;
    let result = on_drive(&drive, rclone, pool, op);
    let keep = unpublished(&drive).unwrap_or(true);
    drop(drive);
    let retry = format!(
        "unpublished changes are kept in {}; finish with `rpool mount --virtual-drive --pool-sync --sync-only --pool {pool} --workspace \"{}\"`",
        dir.display(),
        dir.display()
    );
    if !keep {
        let _ = fs::remove_dir_all(&dir);
    }
    match result {
        Ok((value, mut notes)) => {
            if keep {
                notes.push(retry);
            }
            Ok((value, notes))
        }
        Err(error) if keep => Err(error.context(retry)),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::model::MountEntry;

    fn mount(pool: &str, workspace: &str) -> MountEntry {
        MountEntry {
            id: "aa".into(),
            pool: pool.into(),
            workspace: workspace.into(),
            mountpoint: "R:".into(),
            frontend: "dav".into(),
            pid: 1,
            started_unix: 1,
        }
    }

    #[test]
    fn mounted_workspaces_get_requests_otherwise_workspace_or_cloud() {
        let ws = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = ws.path().to_string_lossy().to_string();
        let mounts = vec![mount("p", &path), mount("replica", "/elsewhere")];
        let drive = |p: &Path| p != Path::new("/elsewhere");
        assert_eq!(
            route("p", None, &mounts, &drive),
            Route::Mount(ws.path().into())
        );
        assert_eq!(
            route("q", Some(ws.path()), &mounts, &drive),
            Route::Mount(ws.path().into())
        );
        assert_eq!(
            route("p", Some(other.path()), &mounts, &drive),
            Route::Workspace(other.path().into())
        );
        assert_eq!(route("q", None, &mounts, &drive), Route::Cloud);
        // A mount that is not an online drive never answers: not used.
        assert_eq!(route("replica", None, &mounts, &drive), Route::Cloud);
        assert_eq!(route("p", None, &[], &drive), Route::Cloud);
    }

    #[test]
    fn worker_label_is_a_portable_name() {
        let worker = cloud_worker();
        assert!(worker.starts_with("rpool-"));
        crate::mount::history_bridge::valid_path(&worker).unwrap();
    }
}
