//! End to end with the installed rclone over local crypt remotes: a pool
//! that gains an account is mounted again without a transition, its drive
//! metadata is copied to the new account, and a new PC on the larger pool
//! sees the files. Removing an account is still refused.
//! Run alone: `cargo test --bin rpool mount::add_accounts_tests -- --ignored --test-threads=1`.
use super::virtual_drive::{Revision, VirtualDrive};
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

/// Three local crypt remotes `c1..c3`; the pool starts on `c1`, `c2`.
fn setup(temp: &Path, rclone: &str) {
    let config_dir = temp.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let mut text = String::new();
    for i in 1..=3 {
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
    let conf = temp.join("rclone.conf");
    fs::write(&conf, text).unwrap();
    // Ignored test, run single-threaded: nothing else reads these.
    std::env::set_var("RCLONE_CONFIG", &conf);
    std::env::set_var("RPOOL_CONFIG_DIR", &config_dir);
    crate::storage::admin::domains::update(
        &["b1=a1".into(), "b2=a2".into(), "b3=a3".into()],
        &["b1=g1".into(), "b2=g2".into(), "b3=g3".into()],
    )
    .unwrap();
    set_remotes(&["c1:", "c2:"]);
}

fn set_remotes(remotes: &[&str]) {
    let mut store = crate::pool::load_pool_store().unwrap();
    store.pools.insert(
        "p".into(),
        PoolDefinition {
            remotes: remotes.iter().map(|r| r.to_string()).collect(),
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
            data_shards: 1,
            parity_shards: 1,
            ..PoolDefinition::default()
        },
    );
    crate::pool::save_pool_store(&store).unwrap();
}

fn write(drive: &VirtualDrive, path: &str, bytes: &[u8]) {
    let intent = drive.begin_observed(path, None).unwrap();
    fs::write(drive.spool_path(&intent), bytes).unwrap();
    drive.seal(intent).unwrap();
    drive.sync().unwrap();
}

fn read(drive: &VirtualDrive, path: &str) -> Vec<u8> {
    drive.pull().unwrap();
    let view = drive.view().unwrap();
    let revision = view.get(path).unwrap_or_else(|| panic!("{path} missing"));
    assert!(matches!(revision, Revision::Cloud { .. }));
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < revision.size() {
        let chunk = drive.read(revision, bytes.len() as u64, 1 << 20).unwrap();
        assert!(!chunk.is_empty());
        bytes.extend(chunk);
    }
    bytes
}

/// Event ids in the drive metadata of crypt remote `remote`.
fn events_on(rclone: &str, remote: &str) -> BTreeSet<String> {
    let root = &super::pool_sync::roots("p", &[remote.to_string()]).unwrap()[0];
    let transport = super::shared_transport::SharedTransport::new(rclone, root).unwrap();
    transport
        .entries(&Default::default(), None)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn adding_an_account_copies_metadata_without_a_transition() {
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path(), &rclone);
    let a = temp.path().join("pc-a");
    {
        let drive = VirtualDrive::open_for_mount(&rclone, "p", &a, "pc-a", 64 << 20).unwrap();
        write(&drive, "one.txt", b"first file");
        write(&drive, "two.txt", b"second file");
    }
    let before = events_on(&rclone, "c1:");
    assert!(!before.is_empty());
    assert!(events_on(&rclone, "c3:").is_empty());

    // The pool gains c3: other openers still refuse, the mount path adds it.
    set_remotes(&["c1:", "c2:", "c3:"]);
    let refused = VirtualDrive::open(&rclone, "p", &a, "pc-a", 64 << 20);
    assert!(refused.is_err());
    {
        let drive = VirtualDrive::open_for_mount(&rclone, "p", &a, "pc-a", 64 << 20).unwrap();
        assert_eq!(drive.pool_sync_roots.len(), 3);
        assert!(events_on(&rclone, "c3:").is_superset(&before));
        assert_eq!(read(&drive, "one.txt"), b"first file");
        // New writes go to every replica, the new account included.
        write(&drive, "three.txt", b"third file");
        assert_eq!(events_on(&rclone, "c3:"), events_on(&rclone, "c1:"));
    }
    // Mounting again is an ordinary open (nothing left to copy).
    drop(VirtualDrive::open_for_mount(&rclone, "p", &a, "pc-a", 64 << 20).unwrap());

    // A new PC on the larger pool sees everything.
    let b = temp.path().join("pc-b");
    let drive = VirtualDrive::open_for_mount(&rclone, "p", &b, "pc-b", 64 << 20).unwrap();
    assert_eq!(read(&drive, "two.txt"), b"second file");
    assert_eq!(read(&drive, "three.txt"), b"third file");
    drop(drive);

    // Removing an account still needs Apply pool changes.
    set_remotes(&["c1:", "c3:"]);
    let Err(error) = VirtualDrive::open_for_mount(&rclone, "p", &a, "pc-a", 64 << 20) else {
        panic!("removing an account must be refused");
    };
    assert!(
        format!("{error:#}").contains("Apply pool changes"),
        "{error:#}"
    );
}
