//! Mount uploads into a `native_crypt` pool, end to end with the installed
//! rclone over local directories (no OS mount, no cloud): RPool encrypts shard
//! and manifest objects itself, names included, and the committed cloud
//! revision reads back through the rclone crypt remotes.
//! Run alone: `cargo test --bin rpool native_crypt_mount -- --ignored --test-threads=1`.
use super::fs_core::{Access, FsCore};
use super::virtual_drive::fixture;
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

fn run(rclone: &str, args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new(rclone)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn native_crypt_mount_uploads_encrypted_objects_readable_through_rclone_crypt() {
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    let config_dir = temp.path().join("config");
    fs::create_dir(&config_dir).unwrap();
    let conf = temp.path().join("rclone.conf");
    let mut text = String::new();
    for i in 1..=3 {
        let data = temp.path().join(format!("data{i}"));
        fs::create_dir(&data).unwrap();
        let obscured = String::from_utf8(run(&rclone, &["obscure", &format!("pw{i}")])).unwrap();
        text.push_str(&format!(
            "[b{i}]\ntype = local\n\n[c{i}]\ntype = crypt\nremote = b{i}:{}\npassword = {}\n\n",
            data.display(),
            obscured.trim()
        ));
    }
    // A crypt remote native crypt refuses (base32768 names): proves routing.
    let data4 = temp.path().join("data4");
    fs::create_dir(&data4).unwrap();
    let obscured = String::from_utf8(run(&rclone, &["obscure", "pw4"])).unwrap();
    text.push_str(&format!(
        "[b4]\ntype = local\n\n[c4]\ntype = crypt\nremote = b4:{}\npassword = {}\nfilename_encoding = base32768\n\n",
        data4.display(),
        obscured.trim()
    ));
    fs::write(&conf, text).unwrap();
    // SAFETY: ignored test, run single-threaded; nothing else reads these.
    unsafe {
        std::env::set_var("RCLONE_CONFIG", &conf);
        std::env::set_var("RPOOL_CONFIG_DIR", &config_dir);
    }
    crate::storage::admin::domains::update(
        &[
            "b1=a1".into(),
            "b2=a2".into(),
            "b3=a3".into(),
            "b4=a4".into(),
        ],
        &[
            "b1=g1".into(),
            "b2=g2".into(),
            "b3=g3".into(),
            "b4=g4".into(),
        ],
    )
    .unwrap();

    let root = temp.path().join("ws");
    fs::create_dir(&root).unwrap();
    let mut drive = fixture(&root);
    drive.rclone = rclone.clone();
    drive.policy.remotes = vec!["c1:".into(), "c2:".into(), "c3:".into()];
    drive.policy.data_shards = 2;
    drive.policy.parity_shards = 1;
    drive.policy.shard_size = crate::models::shard_size::ShardSize::from_mib(1).unwrap();
    drive.policy.native_crypt = true;
    drive.cache = super::shard_cache::ShardCache::new(root.join("clean-cache"), 64 << 20).unwrap();
    let drive = Arc::new(drive);
    let core = FsCore::new(drive.clone()).unwrap();
    let content: Vec<u8> = (0..2_500_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let marker = b"PLAINTEXT-MARKER-0123456789";
    let mut body = marker.to_vec();
    body.extend_from_slice(&content);
    let handle = core
        .open(
            "docs/secret-report.bin",
            Access::Write {
                truncate: true,
                append: false,
            },
            true,
            false,
        )
        .unwrap();
    core.write_at(handle, 0, &body).unwrap();
    core.release(handle).unwrap();

    drive.sync().unwrap();
    assert!(
        drive.state.lock().unwrap().pending.is_empty(),
        "everything uploaded"
    );

    let mut objects = Vec::new();
    for i in 1..=3 {
        objects.extend(files(&temp.path().join(format!("data{i}"))));
    }
    assert!(!objects.is_empty());
    for path in &objects {
        let name = path.to_string_lossy();
        assert!(
            !name.contains("secret-report") && !name.contains("manifest"),
            "plaintext name: {name}"
        );
        let bytes = fs::read(path).unwrap();
        assert!(bytes.starts_with(b"RCLONE\0\0"), "not crypt framed: {name}");
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker),
            "plaintext content in {name}"
        );
    }
    // The committed revision is read back through the rclone crypt remotes.
    let view = drive.view().unwrap();
    let revision = view.get("docs/secret-report.bin").expect("committed file");
    assert!(matches!(
        revision,
        super::virtual_drive::Revision::Cloud { .. }
    ));
    let mut read = Vec::new();
    while (read.len() as u64) < revision.size() {
        let chunk = drive.read(revision, read.len() as u64, 1 << 20).unwrap();
        assert!(!chunk.is_empty());
        read.extend(chunk);
    }
    assert_eq!(read, body);
    let listed = String::from_utf8(run(
        &rclone,
        &["--config", conf.to_str().unwrap(), "lsf", "-R", "c1:"],
    ))
    .unwrap();
    assert!(
        !listed.is_empty(),
        "rclone crypt lists RPool's native objects"
    );

    // base32768 crypt remote: native and rclone crypt writes both succeed and
    // rclone crypt lists the (non-ASCII) stored names.
    for native in [true, false] {
        let root = temp.path().join(format!("ws-control-{native}"));
        fs::create_dir(&root).unwrap();
        let mut drive = fixture(&root);
        drive.rclone = rclone.clone();
        drive.policy.remotes = vec!["c4:".into()];
        drive.policy.data_shards = 1;
        drive.policy.parity_shards = 0;
        drive.policy.shard_size = crate::models::shard_size::ShardSize::from_mib(1).unwrap();
        drive.policy.native_crypt = native;
        let drive = Arc::new(drive);
        let core = FsCore::new(drive.clone()).unwrap();
        let handle = core
            .open(
                "control.txt",
                Access::Write {
                    truncate: true,
                    append: false,
                },
                true,
                false,
            )
            .unwrap();
        core.write_at(handle, 0, b"control").unwrap();
        core.release(handle).unwrap();
        drive.sync().unwrap();
        assert_eq!(
            drive.state.lock().unwrap().pending.len(),
            0,
            "native={native}"
        );
        assert!(fs::read_dir(&data4).unwrap().next().is_some(), "written");
        let listed = String::from_utf8(run(
            &rclone,
            &["--config", conf.to_str().unwrap(), "lsf", "-R", "c4:"],
        ))
        .unwrap();
        assert!(listed.contains("virtual-"), "native={native}: {listed}");
    }
}

#[test]
#[ignore = "requires rclone; sets process environment"]
fn small_files_upload_as_one_pack_and_read_back() {
    let rclone = rclone();
    let temp = tempfile::tempdir().unwrap();
    let config_dir = temp.path().join("config");
    fs::create_dir(&config_dir).unwrap();
    let conf = temp.path().join("rclone.conf");
    let mut text = String::new();
    for i in 1..=3 {
        let data = temp.path().join(format!("data{i}"));
        fs::create_dir(&data).unwrap();
        let obscured = String::from_utf8(run(&rclone, &["obscure", &format!("pw{i}")])).unwrap();
        text.push_str(&format!(
            "[b{i}]\ntype = local\n\n[c{i}]\ntype = crypt\nremote = b{i}:{}\npassword = {}\n\n",
            data.display(),
            obscured.trim()
        ));
    }
    fs::write(&conf, text).unwrap();
    // SAFETY: ignored test, run single-threaded; nothing else reads these.
    unsafe {
        std::env::set_var("RCLONE_CONFIG", &conf);
        std::env::set_var("RPOOL_CONFIG_DIR", &config_dir);
    }
    crate::storage::admin::domains::update(
        &["b1=a1".into(), "b2=a2".into(), "b3=a3".into()],
        &["b1=g1".into(), "b2=g2".into(), "b3=g3".into()],
    )
    .unwrap();
    for native in [false, true] {
        let root = temp.path().join(format!("ws-{native}"));
        fs::create_dir(&root).unwrap();
        let mut drive = fixture(&root);
        drive.rclone = rclone.clone();
        drive.policy.remotes = vec!["c1:".into(), "c2:".into(), "c3:".into()];
        drive.policy.data_shards = 2;
        drive.policy.parity_shards = 1;
        drive.policy.shard_size = crate::models::shard_size::ShardSize::from_mib(1).unwrap();
        drive.policy.native_crypt = native;
        drive.policy.small_file_packing = true;
        drive.cache =
            super::shard_cache::ShardCache::new(root.join("clean-cache"), 64 << 20).unwrap();
        let drive = Arc::new(drive);
        let core = FsCore::new(drive.clone()).unwrap();
        // Small files of awkward sizes, one that crosses the 1 MiB member
        // limit (uploaded on its own), all written before one sync.
        let mut expected = BTreeMap::new();
        for (i, len) in [1usize, 17, 4096, 65_537, 200_000, 1 << 20, (1 << 20) + 1]
            .into_iter()
            .enumerate()
        {
            let path = format!("small/f{i}-{len}.bin");
            let body: Vec<u8> = (0..len)
                .map(|j| {
                    (j as u32)
                        .wrapping_mul(2_654_435_761)
                        .wrapping_add(i as u32) as u8
                })
                .collect();
            let handle = core
                .open(
                    &path,
                    Access::Write {
                        truncate: true,
                        append: false,
                    },
                    true,
                    false,
                )
                .unwrap();
            core.write_at(handle, 0, &body).unwrap();
            core.release(handle).unwrap();
            expected.insert(path, body);
        }
        drive.sync().unwrap();
        assert!(
            drive.state.lock().unwrap().pending.is_empty(),
            "everything uploaded"
        );
        let view = drive.view().unwrap();
        let mut archives = BTreeSet::new();
        let mut packed = 0;
        for (path, body) in &expected {
            let revision = view.get(path).expect("committed");
            let super::virtual_drive::Revision::Cloud { content, .. } = revision else {
                panic!("{path} not in the cloud");
            };
            archives.insert(content.manifest.archive_id.clone());
            packed += usize::from(content.pack.is_some());
            let mut read = Vec::new();
            while (read.len() as u64) < revision.size() {
                let chunk = drive.read(revision, read.len() as u64, 300_000).unwrap();
                assert!(!chunk.is_empty(), "{path}");
                read.extend(chunk);
            }
            assert_eq!(&read, body, "{path} reads back exactly (native {native})");
        }
        // Six small files share one pack; the 1 MiB + 1 file has its own.
        assert_eq!(packed, 6, "native {native}");
        assert_eq!(archives.len(), 2, "native {native}: {archives:?}");
        assert!(archives.iter().any(|id| id.starts_with("virtual-pack-")));
        // Committed pack events are version 3 and valid.
        for event in drive.state.lock().unwrap().events.values() {
            event.validate().unwrap();
            let is_packed = event.content.as_ref().is_some_and(|c| c.pack.is_some());
            assert_eq!(event.version == 3, is_packed);
        }
        assert!(
            !root.join("packs").exists()
                || fs::read_dir(root.join("packs")).unwrap().next().is_none(),
            "pack staging removed after commit"
        );
    }
}
