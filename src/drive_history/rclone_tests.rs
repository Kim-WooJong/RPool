//! End to end with the installed rclone over local crypt remotes (no cloud,
//! no OS mount): two PCs (workspaces), workspace-less cloud listing and
//! restore, a workspace rollback, a request answered by a "mounted" drive,
//! purge marks.
//! Run alone: `cargo test --bin rpool drive_history::rclone_tests -- --ignored --test-threads=1`.
use super::dispatch::run;
use super::model::{RollbackPlan, TrashEntry, VersionEntry};
use super::ops::{Op, PurgeReport, RestoreReport};
use crate::mount::history_bridge::{Revision, VirtualDrive};
use crate::prelude::*;

fn rclone() -> String {
    std::env::var("RPOOL_TEST_RCLONE").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone"
        } else {
            "rclone"
        }
        .into()
    })
}

fn setup(temp: &Path, rclone: &str, pools: &[&str]) {
    let config_dir = temp.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let conf = temp.join("rclone.conf");
    let mut text = String::new();
    for i in 1..=2 {
        let data = temp.join(format!("data{i}"));
        fs::create_dir_all(&data).unwrap();
        let out = std::process::Command::new(rclone)
            .args(["obscure", &format!("pw{i}")])
            .output()
            .unwrap();
        text.push_str(&format!(
            "[b{i}]\ntype = local\n\n[c{i}]\ntype = crypt\nremote = b{i}:{}\npassword = {}\n\n",
            data.display(),
            String::from_utf8(out.stdout).unwrap().trim()
        ));
    }
    fs::write(&conf, text).unwrap();
    // Ignored test, run single-threaded: nothing else reads these.
    std::env::set_var("RCLONE_CONFIG", &conf);
    std::env::set_var("RPOOL_CONFIG_DIR", &config_dir);
    crate::storage::admin::domains::update(
        &["b1=a1".into(), "b2=a2".into()],
        &["b1=g1".into(), "b2=g2".into()],
    )
    .unwrap();
    let mut store = crate::pool::load_pool_store().unwrap();
    for pool in pools {
        store.pools.insert(
            pool.to_string(),
            PoolDefinition {
                remotes: vec!["c1:".into(), "c2:".into()],
                shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
                data_shards: 1,
                parity_shards: 1,
                ..PoolDefinition::default()
            },
        );
    }
    crate::pool::save_pool_store(&store).unwrap();
}

fn open(rclone: &str, pool: &str, root: &Path, worker: &str) -> VirtualDrive {
    let drive = VirtualDrive::open(rclone, pool, root, worker, 64 << 20).unwrap();
    fs::write(root.join("pool-worker.json"), format!("\"{worker}\"")).unwrap();
    drive
}

fn write(drive: &VirtualDrive, path: &str, bytes: &[u8]) {
    // A new editor session reads the current version first.
    drive.peer_read_pins.lock().unwrap().clear();
    let visible = drive.view().unwrap().get(path).cloned();
    if let Some(revision) = &visible {
        drive.pin_read(path, revision).unwrap();
    }
    let intent = drive.begin_observed(path, visible.as_ref()).unwrap();
    fs::write(drive.spool_path(&intent), bytes).unwrap();
    drive.seal(intent).unwrap();
    drive.sync().unwrap();
    // Distinct record times (listing ModTimes have one-second resolution here).
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn read(drive: &VirtualDrive, path: &str) -> Vec<u8> {
    drive.pull().unwrap();
    let view = drive.view().unwrap();
    let revision = view
        .get(path)
        .unwrap_or_else(|| panic!("{path} missing: {view:?}"));
    assert!(matches!(revision, Revision::Cloud { .. }));
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < revision.size() {
        let chunk = drive.read(revision, bytes.len() as u64, 1 << 20).unwrap();
        assert!(!chunk.is_empty());
        bytes.extend(chunk);
    }
    bytes
}

fn op<T: serde::de::DeserializeOwned>(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    op: Op,
) -> T {
    let (value, notes) = run(rclone, pool, workspace, &op).unwrap();
    for note in notes {
        eprintln!("note: {note}");
    }
    serde_json::from_value(value).unwrap()
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn v6_trash_versions_rollback_and_mount_requests_end_to_end() {
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path(), &rclone, &["hist"]);
    let ws_a = temp.path().join("pc-a");
    let a = open(&rclone, "hist", &ws_a, "PC-A");
    write(&a, "Docs/a.txt", b"version one");
    write(&a, "Docs/a.txt", b"version two");
    write(&a, "keep.txt", b"keep");
    a.delete("Docs/a.txt").unwrap();
    a.sync().unwrap();
    drop(a);

    // Without any workspace: the cloud listing has the deletion with its time.
    let trash: Vec<TrashEntry> = op(&rclone, "hist", None, Op::TrashList);
    assert_eq!(trash.len(), 1, "{trash:?}");
    assert_eq!(trash[0].path, "/Docs/a.txt");
    assert_eq!(trash[0].deleted_by.as_deref(), Some("PC-A"));
    assert!(trash[0].deleted_unix.is_some() && trash[0].expires_unix.is_some());
    let versions: Vec<VersionEntry> = op(
        &rclone,
        "hist",
        None,
        Op::VersionsList {
            path: "/Docs/a.txt".into(),
        },
    );
    assert_eq!(versions.len(), 3);
    assert!(versions.windows(2).all(|w| w[0].time_unix > w[1].time_unix));
    let first = versions[2].clone();

    // Restore from the cloud (scratch workspace, this PC's worker).
    let restored: RestoreReport = op(
        &rclone,
        "hist",
        None,
        Op::TrashRestore {
            ids: vec![trash[0].id.clone()],
            to: None,
            into: None,
        },
    );
    assert!(restored.published, "{restored:?}");
    let ws_b = temp.path().join("pc-b");
    let b = open(&rclone, "hist", &ws_b, "PC-B");
    assert_eq!(read(&b, "Docs/a.txt"), b"version two");
    drop(b);
    let trash: Vec<TrashEntry> = op(&rclone, "hist", None, Op::TrashList);
    assert!(trash.is_empty());

    // Rollback through the unmounted workspace of PC-A: back to version one;
    // keep.txt (created later) goes to the trash.
    let at = first.time_unix.unwrap();
    let preview: RollbackPlan = op(
        &rclone,
        "hist",
        Some(&ws_a),
        Op::Rollback {
            path: "/".into(),
            at,
            confirm: false,
        },
    );
    assert_eq!(preview.changes.len(), 2, "{preview:?}");
    let applied: RollbackPlan = op(
        &rclone,
        "hist",
        Some(&ws_a),
        Op::Rollback {
            path: "/".into(),
            at,
            confirm: true,
        },
    );
    assert!(applied.applied);
    let b = open(&rclone, "hist", &ws_b, "PC-B");
    assert_eq!(read(&b, "Docs/a.txt"), b"version one");
    assert!(!b.view().unwrap().contains_key("keep.txt"));
    drop(b);

    // A "mounted" PC-A answers requests from its maintenance loop.
    let a = Arc::new(open(&rclone, "hist", &ws_a, "PC-A"));
    let registry = crate::monitor::registry::registry_dir().unwrap();
    let entry = crate::monitor::model::MountEntry {
        id: crate::monitor::registry::new_id(),
        pool: "hist".into(),
        workspace: ws_a.to_string_lossy().into_owned(),
        mountpoint: "R:".into(),
        frontend: "dav".into(),
        pid: std::process::id(),
        started_unix: 1,
    };
    let registered = crate::monitor::registry::register(&registry, &entry).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = {
        let (a, stop) = (a.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                super::dispatch::serve_mount(&a).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
    };
    let trash: Vec<TrashEntry> = op(&rclone, "hist", None, Op::TrashList);
    assert_eq!(
        trash.len(),
        1,
        "keep.txt removed by the rollback: {trash:?}"
    );
    assert_eq!(trash[0].path, "/keep.txt");
    let preview: PurgeReport = op(
        &rclone,
        "hist",
        None,
        Op::TrashPurge {
            ids: vec![trash[0].id.clone()],
            expired: false,
            all: false,
            confirm: false,
        },
    );
    assert!(!preview.applied && preview.eligible_bytes > 0);
    let purged: PurgeReport = op(
        &rclone,
        "hist",
        None,
        Op::TrashPurge {
            ids: vec![trash[0].id.clone()],
            expired: false,
            all: false,
            confirm: true,
        },
    );
    assert!(purged.applied);
    stop.store(true, std::sync::atomic::Ordering::Release);
    server.join().unwrap();
    fs::remove_file(registered).unwrap();
    drop(a);
    // The purge mark hides the entry for every PC (here: the cloud listing).
    let trash: Vec<TrashEntry> = op(&rclone, "hist", None, Op::TrashList);
    assert!(trash.is_empty(), "{trash:?}");
}

/// Top-level `virtual-*` folders stored on the pool's accounts.
fn drive_folders(rclone: &str) -> BTreeSet<String> {
    let roots: BTreeSet<String> = ["c1:".to_string(), "c2:".to_string()].into();
    let listings = crate::migration::retire::observe::list_roots(rclone, &roots);
    let mut out = BTreeSet::new();
    for listing in listings.values() {
        let files = listing.files().expect("listed");
        for path in files.keys() {
            let first = path.split('/').next().unwrap();
            if first.starts_with("virtual-") {
                out.insert(first.to_owned());
            }
        }
    }
    out
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn v6_cleanup_deletes_unreferenced_archives_after_the_grace_period() {
    use super::cleanup::{execute, live::LiveIo, select::folders};
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path(), &rclone, &["gc"]);
    super::retention::update("gc", Some(1), Some(1), Some(1)).unwrap();
    let ws_a = temp.path().join("pc-a");
    let a = open(&rclone, "gc", &ws_a, "PC-A");
    write(&a, "f.txt", b"one");
    write(&a, "f.txt", b"two");
    write(&a, "f.txt", b"three");
    write(&a, "keep.txt", b"kept file");
    write(&a, "gone.txt", b"deleted file");
    a.delete("gone.txt").unwrap();
    write(&a, "purge.txt", b"purged file");
    a.delete("purge.txt").unwrap();
    a.sync().unwrap();
    drop(a);
    let trash: Vec<TrashEntry> = op(&rclone, "gc", None, Op::TrashList);
    let purge = trash.iter().find(|e| e.path == "/purge.txt").unwrap();
    let purged: PurgeReport = op(
        &rclone,
        "gc",
        None,
        Op::TrashPurge {
            ids: vec![purge.id.clone()],
            expired: false,
            all: false,
            confirm: true,
        },
    );
    assert!(purged.applied);
    let before = drive_folders(&rclone);
    let versions: Vec<VersionEntry> = op(
        &rclone,
        "gc",
        None,
        Op::VersionsList {
            path: "/f.txt".into(),
        },
    );
    assert!(versions.iter().all(|v| v.restorable));

    let retention = super::retention::load("gc").unwrap();
    let settings = super::retention::load_cleanup("gc").unwrap();
    let mut options = execute::Options::new(retention, settings);
    // A preview writes nothing.
    let mut io = LiveIo::open(&rclone, "gc", None).unwrap();
    io.clock_offset = 10 * super::retention::DAY;
    let preview = execute::run(&io, "gc", &options).unwrap();
    assert!(preview.postponed.is_empty(), "{preview:?}");
    assert!(preview.candidates.files >= 4, "{preview:?}");
    // Ten days later: marked, nothing deleted.
    options.confirm = true;
    let marked = execute::run(&io, "gc", &options).unwrap();
    assert_eq!(marked.candidates, preview.candidates);
    assert_eq!(marked.deleted.archives, 0);
    assert_eq!(drive_folders(&rclone), before);
    // Past the grace: deleted (only the current files remain, which is more
    // than the guard's share, so it is forced).
    io.clock_offset = 20 * super::retention::DAY;
    let refused = execute::run(&io, "gc", &options).unwrap();
    assert!(
        refused.guard.is_some() && refused.deleted.archives == 0,
        "{refused:?}"
    );
    options.force = true;
    let deleted = execute::run(&io, "gc", &options).unwrap();
    assert_eq!(
        deleted.deleted.archives, marked.candidates.archives,
        "{deleted:?}"
    );
    let after = drive_folders(&rclone);
    assert!(after.len() < before.len());

    // Current files read back; their archives are all that is left.
    let b = open(&rclone, "gc", &temp.path().join("pc-b"), "PC-B");
    assert_eq!(read(&b, "f.txt"), b"three");
    assert_eq!(read(&b, "keep.txt"), b"kept file");
    let mut kept = BTreeSet::new();
    for revision in b.view().unwrap().values() {
        if let Revision::Cloud { content, .. } = revision {
            kept.extend(folders(&content.manifest));
        }
    }
    drop(b);
    assert!(after.is_subset(&kept), "{after:?} vs {kept:?}");
    // Old versions are no longer offered for restore.
    let versions: Vec<VersionEntry> = op(
        &rclone,
        "gc",
        None,
        Op::VersionsList {
            path: "/f.txt".into(),
        },
    );
    assert!(versions.iter().any(|v| !v.restorable && !v.current));
    assert!(versions.iter().any(|v| v.restorable && v.current));
    // Nothing left to do.
    let again = execute::run(&io, "gc", &options).unwrap();
    assert_eq!(
        (
            again.candidates.archives,
            again.due.archives,
            again.deleted.archives
        ),
        (0, 0, 0),
        "{again:?}"
    );
}
