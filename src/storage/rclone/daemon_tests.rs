//! Daemon transport against the installed rclone and a temporary local crypt
//! remote. `RPOOL_TEST_RCLONE` overrides the binary. Each test has its own
//! config file, hence its own daemon.
use super::*;
use crate::crypt::obscure::obscure;
use crate::storage::error::StorageErrorKind;
use std::time::Instant;

fn rclone() -> PathBuf {
    std::env::var_os("RPOOL_TEST_RCLONE")
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/rclone".into()
            } else {
                "rclone".into()
            }
        })
        .into()
}

struct Fixture {
    temp: tempfile::TempDir,
    base: String,
    daemon: RcloneContext,
    subprocess: RcloneContext,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let conf = temp.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[base]\ntype = local\n\n[c]\ntype = crypt\nremote = base:{}\npassword = {}\n",
            data.display(),
            obscure("daemon test").unwrap()
        ),
    )
    .unwrap();
    let context = |daemon: bool| {
        let mut context = RcloneContext::new(rclone(), ConfigSelection::File(conf.clone()));
        context.environment.retain(|(k, _)| {
            let k = k.to_string_lossy().to_ascii_uppercase();
            !k.starts_with("RCLONE_") && !k.starts_with("RPOOL_RCLONE")
        });
        if daemon {
            context.allow_daemon_for_test();
        }
        context
    };
    let fixture = Fixture {
        base: format!("base:{}", temp.path().display()),
        temp,
        daemon: context(true),
        subprocess: context(false),
    };
    let ctx = OperationContext::none();
    let put = |address: &str, bytes: &[u8]| {
        fixture
            .subprocess
            .write_raw(
                &ctx,
                address,
                &mut &bytes[..],
                Some(bytes.len() as u64),
                &WriteOptions::default(),
            )
            .unwrap()
    };
    put("c:dir/sub/file.bin", &sample(100_000));
    put("c:dir/sp ace [x]%20 한.txt", b"hi");
    put("c:dir/empty", b"");
    std::fs::write(fixture.temp.path().join("plain.bin"), sample(1234)).unwrap();
    fixture
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn same<T: PartialEq + std::fmt::Debug>(
    daemon: Result<T, StorageError>,
    subprocess: Result<T, StorageError>,
    what: &str,
) {
    match (daemon, subprocess) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "{what}"),
        (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind(), "{what}: {a} vs {b}"),
        (a, b) => panic!(
            "{what}: daemon {:?} vs subprocess {:?}",
            a.is_ok(),
            b.is_ok()
        ),
    }
}

fn listing(bytes: Vec<u8>) -> Vec<(String, u64, bool)> {
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let mut entries: Vec<_> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["Path"].as_str().unwrap().to_owned(),
                e["Size"].as_i64().unwrap() as u64,
                e["IsDir"].as_bool().unwrap(),
            )
        })
        .collect();
    entries.sort();
    entries
}

fn read(
    context: &RcloneContext,
    address: &str,
    range: Option<(u64, u64)>,
) -> Result<Vec<u8>, StorageError> {
    let range = range.map(|(o, l)| ReadRange::new(o, l).unwrap());
    let mut bytes = Vec::new();
    let receipt = context.read_raw(
        &OperationContext::none(),
        address,
        range.as_ref(),
        &mut bytes,
    )?;
    assert_eq!(receipt.bytes_read, bytes.len() as u64);
    Ok(bytes)
}

fn kill(pid: u32) {
    assert!(std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_millis(200));
}

#[test]
#[ignore = "requires rclone"]
fn daemon_reads_equal_subprocess_reads() {
    let f = fixture();
    let ctx = OperationContext::none();
    assert!(daemon::get(&f.daemon).is_some(), "daemon starts");
    assert!(daemon::get(&f.subprocess).is_none());
    // The daemon itself answers these (no silent fallback).
    let d = daemon::get(&f.daemon).unwrap();
    for address in [
        "c:dir/sub/file.bin",
        "c:dir",
        "c:",
        "c:dir/sp ace [x]%20 한.txt",
    ] {
        assert!(d.stat(&ctx, address, json!({})).is_ok(), "{address}");
    }
    for address in ["c:dir", "c:", "c:dir/sub"] {
        assert!(d.list(&ctx, address, LIST_LIMIT).is_ok(), "{address}");
    }
    let (mut bytes, mut written) = (Vec::new(), 0);
    assert!(d
        .read(
            &ctx,
            "c:dir/sp ace [x]%20 한.txt",
            0,
            None,
            &mut bytes,
            &mut written
        )
        .is_ok());
    assert_eq!((bytes.as_slice(), written), (&b"hi"[..], 2));
    let (mut bytes, mut written) = (Vec::new(), 0);
    assert!(d
        .read(
            &ctx,
            "c:dir/sub/file.bin",
            99_990,
            Some(100),
            &mut bytes,
            &mut written
        )
        .is_ok());
    assert_eq!(bytes, sample(100_000)[99_990..]);
    let item = d
        .stat(
            &ctx,
            &format!("{}/plain.bin", f.base),
            json!({"showHash": true, "hashTypes": ["md5"]}),
        )
        .ok()
        .unwrap();
    assert!(item["Hashes"]["md5"].is_string());
    for address in [
        "c:dir/sub/file.bin",
        "c:dir/sp ace [x]%20 한.txt",
        "c:dir/empty",
        "c:dir",
        "c:dir/",
        "c:",
        "c:dir/missing",
        "c:nodir/missing",
    ] {
        same(
            f.daemon.stat_raw(&ctx, address),
            f.subprocess.stat_raw(&ctx, address),
            address,
        );
    }
    for (address, range) in [
        ("c:dir/sub/file.bin", None),
        ("c:dir/sub/file.bin", Some((0, 10))),
        ("c:dir/sub/file.bin", Some((100, 100))),
        ("c:dir/sub/file.bin", Some((99_990, 100))),
        ("c:dir/sub/file.bin", Some((200_000, 5))),
        ("c:dir/sp ace [x]%20 한.txt", None),
        ("c:dir/empty", None),
        ("c:dir/missing", None),
        ("c:dir/missing", Some((0, 4))),
        ("c:dir", None),
    ] {
        same(
            read(&f.daemon, address, range),
            read(&f.subprocess, address, range),
            &format!("{address} {range:?}"),
        );
    }
    same(
        f.daemon.read_all_raw(&ctx, "c:dir/sub/file.bin", Some(7)),
        f.subprocess
            .read_all_raw(&ctx, "c:dir/sub/file.bin", Some(7)),
        "read_all limit",
    );
    for address in ["c:dir", "c:dir/", "c:", "c:dir/sub"] {
        same(
            f.daemon.list_recursive(&ctx, address).map(listing),
            f.subprocess.list_recursive(&ctx, address).map(listing),
            address,
        );
    }
    same(
        f.daemon.list_recursive(&ctx, "c:nodir").map(listing),
        f.subprocess.list_recursive(&ctx, "c:nodir").map(listing),
        "missing listing",
    );
    same(
        f.daemon.list_recursive(&ctx, "c:dir/empty").map(listing),
        f.subprocess
            .list_recursive(&ctx, "c:dir/empty")
            .map(listing),
        "file listing",
    );
    let plain = format!("{}/plain.bin", f.base);
    let hashes = vec!["md5".to_owned(), "sha1".to_owned()];
    let hash = f.daemon.object_hash(&ctx, &plain, &hashes).unwrap();
    assert!(hash
        .as_ref()
        .is_some_and(|(size, h)| *size == 1234 && h.starts_with("md5:")));
    same(
        Ok(hash),
        f.subprocess.object_hash(&ctx, &plain, &hashes),
        "hash",
    );
    same(
        f.daemon
            .object_hash(&ctx, &format!("{}/nope", f.base), &hashes),
        f.subprocess
            .object_hash(&ctx, &format!("{}/nope", f.base), &hashes),
        "missing hash",
    );
    // A read after a subprocess write sees the new bytes (no stale cache).
    f.subprocess
        .write_raw(
            &ctx,
            "c:dir/empty",
            &mut &b"now"[..],
            Some(3),
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(read(&f.daemon, "c:dir/empty", None).unwrap(), b"now");
}

#[test]
#[ignore = "requires rclone"]
fn missing_objects_are_definite_daemon_answers() {
    let f = fixture();
    let ctx = OperationContext::none();
    let daemon = daemon::get(&f.daemon).unwrap();
    for address in ["c:dir/missing", "c:nodir/missing"] {
        match daemon.stat(&ctx, address, json!({})) {
            Err(daemon::Failure::Definite(e)) => assert_eq!(e.kind(), StorageErrorKind::NotFound),
            _ => panic!("stat {address} must be a definite NotFound"),
        }
        let mut written = 0;
        match daemon.read(&ctx, address, 0, None, &mut Vec::new(), &mut written) {
            Err(daemon::Failure::Definite(e)) => assert_eq!(e.kind(), StorageErrorKind::NotFound),
            _ => panic!("read {address} must be a definite NotFound"),
        }
    }
    // Past-the-end ranges and directories are not NotFound: the CLI decides.
    let mut written = 0;
    assert!(matches!(
        daemon.read(
            &ctx,
            "c:dir/sub/file.bin",
            200_000,
            Some(5),
            &mut Vec::new(),
            &mut written
        ),
        Err(daemon::Failure::Fallback)
    ));
    assert!(matches!(
        daemon.read(&ctx, "c:dir", 0, None, &mut Vec::new(), &mut written),
        Err(daemon::Failure::Fallback)
    ));
}

#[test]
#[ignore = "requires rclone"]
fn killed_daemon_falls_back_restarts_once_then_stays_on_subprocesses() {
    let f = fixture();
    let expected = sample(100_000);
    let first = daemon::get(&f.daemon).unwrap();
    kill(first.pid());
    // In-flight handle: transport error -> this read falls back.
    let mut written = 0;
    assert!(matches!(
        first.read(
            &OperationContext::none(),
            "c:dir/sub/file.bin",
            0,
            None,
            &mut Vec::new(),
            &mut written
        ),
        Err(daemon::Failure::Fallback)
    ));
    assert_eq!(
        read(&f.daemon, "c:dir/sub/file.bin", None).unwrap(),
        expected
    );
    let second = daemon::get(&f.daemon).expect("restarted once");
    assert_ne!(second.pid(), first.pid());
    kill(second.pid());
    assert_eq!(
        read(&f.daemon, "c:dir/sub/file.bin", None).unwrap(),
        expected
    );
    assert_eq!(
        f.daemon
            .stat_raw(&OperationContext::none(), "c:dir/sub/file.bin")
            .unwrap()
            .size,
        100_000
    );
    assert!(daemon::get(&f.daemon).is_none(), "no second restart");
}

#[test]
#[ignore = "requires rclone"]
fn opt_out_env_and_config_changes() {
    let mut f = fixture();
    f.daemon.set_test_environment("RPOOL_RCLONE_DAEMON", "0");
    assert!(daemon::get(&f.daemon).is_none());
    f.daemon
        .environment
        .retain(|(k, _)| k != "RPOOL_RCLONE_DAEMON");
    let first = daemon::get(&f.daemon).unwrap();
    assert_eq!(daemon::get(&f.daemon).unwrap().pid(), first.pid());
    // Editing the config replaces the daemon, as a subprocess would re-read it.
    let conf = f.temp.path().join("rclone.conf");
    let mut text = std::fs::read_to_string(&conf).unwrap();
    text.push_str("\n[extra]\ntype = local\n");
    std::fs::write(&conf, text).unwrap();
    let second = daemon::get(&f.daemon).unwrap();
    assert_ne!(second.pid(), first.pid());
    assert_eq!(
        read(&f.daemon, "c:dir/sp ace [x]%20 한.txt", None).unwrap(),
        b"hi"
    );
}

/// Manual timing: `cargo test daemon_stat_timing -- --ignored --nocapture`.
#[test]
#[ignore = "requires rclone; timing report"]
fn daemon_stat_timing() {
    let f = fixture();
    let ctx = OperationContext::none();
    daemon::get(&f.daemon).unwrap();
    for (label, context) in [("daemon", &f.daemon), ("subprocess", &f.subprocess)] {
        let started = Instant::now();
        for _ in 0..100 {
            assert_eq!(
                context.stat_raw(&ctx, "c:dir/sub/file.bin").unwrap().size,
                100_000
            );
        }
        let stats = started.elapsed();
        let started = Instant::now();
        for _ in 0..100 {
            assert_eq!(
                read(context, "c:dir/sub/file.bin", Some((5000, 4096)))
                    .unwrap()
                    .len(),
                4096
            );
        }
        eprintln!(
            "{label}: 100 stats {:?} ({:?}/op), 100 ranged reads {:?}",
            stats,
            stats / 100,
            started.elapsed()
        );
    }
}

#[test]
#[ignore = "requires rclone"]
fn daemon_uploads_equal_subprocess_uploads() {
    let f = fixture();
    let ctx = OperationContext::none();
    let d = daemon::get(&f.daemon).unwrap();
    // The daemon itself takes the upload (no silent fallback).
    assert!(d.upload(&ctx, "c:up/direct.bin", &sample(1000)).is_ok());
    assert_eq!(
        read(&f.subprocess, "c:up/direct.bin", None).unwrap(),
        sample(1000)
    );
    let names = [
        "c:up/plain.bin",
        "c:up/sp ace [x]%20 한+b.txt",
        "c:up/deep/er/𝓧ᐊ鵜.bin",
    ];
    // Empty, small, and largest daemon size; then one over: the subprocess.
    let max = daemon::DAEMON_UPLOAD_MAX as usize;
    for (i, len) in [0, 100_000, max, max + 1].into_iter().enumerate() {
        for name in names {
            let address = format!("{name}.{i}");
            let bytes = sample(len);
            let receipt = f
                .daemon
                .write_raw(
                    &ctx,
                    &address,
                    &mut &bytes[..],
                    Some(len as u64),
                    &WriteOptions::default(),
                )
                .unwrap();
            assert_eq!(receipt.size, len as u64, "{address}");
            assert_eq!(
                read(&f.subprocess, &address, None).unwrap(),
                bytes,
                "{address}"
            );
        }
    }
    // Native crypt writes already-encrypted bytes under encoded names to the
    // plain base remote.
    let raw = format!("{}/raw/ꕋꔵ꘠-𐀀.bin", f.base);
    let bytes = sample(4096);
    f.daemon
        .write_ungated(
            &ctx,
            &raw,
            &mut &bytes[..],
            Some(4096),
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(read(&f.subprocess, &raw, None).unwrap(), bytes);
    // A source shorter than announced is refused, exactly like rcat.
    assert!(f
        .daemon
        .write_raw(
            &ctx,
            "c:up/short.bin",
            &mut &sample(10)[..],
            Some(20),
            &WriteOptions::default(),
        )
        .is_err());
}

#[test]
#[ignore = "requires rclone; timing report"]
fn daemon_upload_timing() {
    let f = fixture();
    let ctx = OperationContext::none();
    daemon::get(&f.daemon).unwrap();
    let bytes = sample(64 * 1024);
    for (label, context) in [("daemon", &f.daemon), ("subprocess", &f.subprocess)] {
        let started = Instant::now();
        for i in 0..100 {
            context
                .write_raw(
                    &ctx,
                    &format!("c:timing/{label}-{i}.bin"),
                    &mut &bytes[..],
                    Some(bytes.len() as u64),
                    &WriteOptions::default(),
                )
                .unwrap();
        }
        let took = started.elapsed();
        eprintln!(
            "{label}: 100 uploads of 64 KiB {took:?} ({:?}/upload)",
            took / 100
        );
    }
}

#[test]
#[ignore = "requires rclone"]
fn config_dump_is_cached_until_the_config_file_changes() {
    let f = fixture();
    let ctx = OperationContext::none();
    let first = f.subprocess.config_dump(&ctx).unwrap();
    assert!(first.get("c").is_some() && first.get("added").is_none());
    assert_eq!(f.subprocess.config_dump(&ctx).unwrap(), first, "cached");
    let conf = f.temp.path().join("rclone.conf");
    let mut text = std::fs::read_to_string(&conf).unwrap();
    text.push_str("\n[added]\ntype = local\n");
    std::fs::write(&conf, text).unwrap();
    let after = f.subprocess.config_dump(&ctx).unwrap();
    assert!(after.get("added").is_some(), "a changed file is read again");
}
