//! End to end with the installed rclone over local crypt remotes in a
//! temporary HOME (no cloud, no OS mount): a pool with two drive
//! generations (the second written by a PC that applied a pool change) and
//! a speed-test leftover; list, then clean one provider.
//! Run alone: `cargo test --bin rpool provider::storage::rclone_tests -- --ignored --test-threads=1`.
use super::execute::{run, Request, StorageIo};
use super::live::LiveIo;
use super::model::{CleanMode, GroupKind};
use crate::mount::history_bridge::VirtualDrive;
use crate::prelude::*;

/// rclone executable (`RPOOL_TEST_RCLONE` overrides).
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

/// Runs rclone with the test config; returns stdout.
fn rclone_run(rclone: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(rclone)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rclone {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Two local crypt remotes `c1:`/`c2:`, an isolated HOME and config dir, and
/// pool `RPool` on both.
fn setup(temp: &Path, rclone: &str) {
    let home = temp.join("home");
    let config_dir = temp.join("config");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config_dir).unwrap();
    let conf = temp.join("rclone.conf");
    let mut text = String::new();
    for i in 1..=2 {
        let data = temp.join(format!("data{i}"));
        fs::create_dir_all(&data).unwrap();
        let password = rclone_run(rclone, &["obscure", &format!("pw{i}")]);
        text.push_str(&format!(
            "[b{i}]\ntype = local\n\n[c{i}]\ntype = crypt\nremote = b{i}:{}\npassword = {}\n\n",
            data.display(),
            password.trim()
        ));
    }
    fs::write(&conf, text).unwrap();
    // Ignored test, run single-threaded: nothing else reads these.
    std::env::set_var("HOME", &home);
    std::env::set_var("RCLONE_CONFIG", &conf);
    std::env::set_var("RPOOL_CONFIG_DIR", &config_dir);
    crate::storage::admin::domains::update(
        &["b1=a1".into(), "b2=a2".into()],
        &["b1=g1".into(), "b2=g2".into()],
    )
    .unwrap();
    let mut store = crate::pool::load_pool_store().unwrap();
    store.pools.insert(
        "RPool".into(),
        PoolDefinition {
            remotes: vec!["c1:".into(), "c2:".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
            data_shards: 1,
            parity_shards: 1,
            ..PoolDefinition::default()
        },
    );
    crate::pool::save_pool_store(&store).unwrap();
}

/// Writes `path` on `drive` and publishes it.
fn write(drive: &VirtualDrive, path: &str, bytes: &[u8]) {
    let intent = drive.begin_observed(path, None).unwrap();
    fs::write(drive.spool_path(&intent), bytes).unwrap();
    drive.seal(intent).unwrap();
    drive.sync().unwrap();
    // Distinct record times (listing ModTimes have one-second resolution here).
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

/// Relative paths stored on a crypt remote.
fn stored(rclone: &str, remote: &str) -> Vec<String> {
    rclone_run(rclone, &["lsf", "-R", "--files-only", remote])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn list_and_clean_old_generation_and_speed_test_leftovers() {
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path(), &rclone);
    // PC A writes on the first generation.
    let a = VirtualDrive::open(
        &rclone,
        "RPool",
        &temp.path().join("pc-a"),
        "PC-A",
        64 << 20,
    )
    .unwrap();
    write(&a, "old.txt", b"written on the first generation");
    drop(a);
    // PC B applied a pool change: a new (transition) generation, newer.
    let epoch = crate::migration::execute::random_hex(32).unwrap();
    let ws_b = temp.path().join("pc-b");
    let b =
        VirtualDrive::open_with_epoch(&rclone, "RPool", &ws_b, "PC-B", 64 << 20, &epoch).unwrap();
    write(&b, "new.txt", b"written on the second generation");
    drop(b);
    // An interrupted speed test two days ago.
    let leftover = temp.path().join("leftover");
    fs::write(&leftover, b"speed").unwrap();
    rclone_run(
        &rclone,
        &[
            "copyto",
            leftover.to_str().unwrap(),
            "c1:.rpool-speedtest/run1/f0",
        ],
    );
    rclone_run(
        &rclone,
        &[
            "touch",
            "--timestamp",
            "2020-01-01T00:00:00",
            "c1:.rpool-speedtest/run1/f0",
        ],
    );

    // Right now the first generation was written minutes ago: a PC may still
    // use it, so it is kept.
    let view = super::list(&rclone, "c1").unwrap();
    assert!(view.uncertain.is_empty(), "{:?}", view.uncertain);
    assert!(
        !view
            .groups
            .iter()
            .any(|g| g.kind == GroupKind::OldGeneration),
        "{:#?}",
        view.groups
    );

    // Fifteen days later (clock offset) it is an unconfirmed older generation.
    let later = |days: u64| {
        let mut io = LiveIo::open(&rclone, "c1", 4).unwrap();
        io.clock_offset = days * 86_400;
        io
    };
    let listed = |io: &LiveIo| {
        super::execute::view(&io.observe().unwrap(), io.records(), io.grace_days(), None)
    };
    let io = later(15);
    let view = listed(&io);
    assert!(view.uncertain.is_empty(), "{:?}", view.uncertain);
    assert_eq!(view.pools, vec!["RPool".to_string()]);
    let group = |kind: GroupKind| {
        view.groups
            .iter()
            .find(|g| g.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} group: {:#?}", view.groups))
            .clone()
    };
    let in_use = group(GroupKind::InUse);
    let old = group(GroupKind::OldGeneration);
    let speed = group(GroupKind::SpeedTestLeftovers);
    assert_eq!(old.id, "old:RPool:first");
    assert!(old.deletable && old.needs == vec![super::model::Ack::UnconfirmedGenerations]);
    assert_eq!(old.archives, 1, "{old:#?}");
    assert!(in_use.archives >= 1 && !in_use.deletable);
    assert!(speed.deletable && speed.immediate && speed.units == 1);
    let text = crate::presentation::provider_storage::view_text(&view);
    eprintln!("{text}");
    assert!(text.contains("old:RPool:first"), "{text}");

    // Without the acknowledgement the old generation is refused.
    let mut request = Request {
        groups: vec![old.id.clone(), speed.id.clone()],
        workers: 4,
        ..Default::default()
    };
    let refused = run(&io, &request).unwrap_err();
    assert!(format!("{refused:#}").contains("--include-unconfirmed-generations"));
    request.unconfirmed = true;
    let preview = run(&io, &request).unwrap();
    assert_eq!(preview.mode, CleanMode::Preview);
    assert_eq!(preview.new.units, 2);
    assert_eq!(preview.due.units, 1, "speed test leftovers are due at once");

    // Confirmed: marks published on the provider, the speed test deleted now.
    request.confirm = true;
    let marked = run(&io, &request).unwrap();
    assert_eq!(marked.mode, CleanMode::Applied);
    assert_eq!(marked.deleted.units, 1);
    let files = stored(&rclone, "c1:");
    assert!(
        !files.iter().any(|f| f.starts_with(".rpool-speedtest/")),
        "{files:?}"
    );
    assert!(files
        .iter()
        .any(|f| f.starts_with(".rpool-sync/storage-cleanup-v1/") && f.ends_with(".json")));
    // Another PC (a fresh open) sees the marks.
    let view = listed(&later(16));
    let pending = view.groups.iter().find(|g| g.id == old.id).unwrap().pending;
    assert_eq!(pending.marked, 2);
    assert!(pending.last_due_unix.unwrap() > io.now());

    // Before the grace period nothing goes; eight days later it is deleted.
    // (The speed-test group is gone: selecting it again is refused.)
    assert!(run(&later(20), &request).is_err());
    request.groups = vec![old.id.clone()];
    let waiting = run(&later(20), &request).unwrap();
    assert_eq!((waiting.waiting.units, waiting.deleted.units), (2, 0));
    let io = later(23);
    request.force = true; // a tiny test provider: half its objects are old
    let done = run(&io, &request).unwrap();
    assert_eq!(done.deleted.units, 2, "{done:#?}");
    eprint!(
        "{}",
        crate::presentation::provider_storage::clean_text(&done)
    );
    let scope = crate::mount::pool_sync::scope("RPool").unwrap();
    let after = stored(&rclone, "c1:");
    let base_meta = format!(".rpool-sync/events-v6/{scope}/events/");
    assert!(
        !after.iter().any(|f| f.starts_with(&base_meta)),
        "{after:?}"
    );
    assert!(after
        .iter()
        .any(|f| f.contains(&format!("epochs/{epoch}/"))));
    // Only this provider was cleaned; the second generation still reads.
    let other = stored(&rclone, "c2:");
    assert!(other.iter().any(|f| f.starts_with(&base_meta)));
    let b = VirtualDrive::open(&rclone, "RPool", &ws_b, "PC-B", 64 << 20).unwrap();
    b.pull().unwrap();
    let view_b = b.view().unwrap();
    let revision = view_b.get("new.txt").expect("new.txt still visible");
    let bytes = b.read(revision, 0, 1 << 20).unwrap();
    assert_eq!(bytes, b"written on the second generation");
    drop(b);
    let view = listed(&later(24));
    assert!(view.uncertain.is_empty(), "{:?}", view.uncertain);
    assert!(
        !view
            .groups
            .iter()
            .any(|g| g.kind == GroupKind::OldGeneration),
        "{:#?}",
        view.groups
    );
    // The command-level entry points work on the real provider too.
    let view = super::list(&rclone, "c1").unwrap();
    assert!(view.uncertain.is_empty() && !view.groups.is_empty());
}
