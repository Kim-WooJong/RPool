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
