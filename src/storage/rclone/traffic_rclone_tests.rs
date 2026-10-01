//! Traffic counters against the installed rclone and temporary local crypt
//! remotes (`RPOOL_TEST_RCLONE` overrides the binary). Remote names are
//! unique to this file, so parallel tests cannot touch these counters.
use super::traffic::snapshot;
use super::*;
use crate::crypt::obscure::obscure;
use crate::monitor::sampler::Sampler;

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

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 17 % 253) as u8).collect()
}

/// `<prefix>_base` (local) and `<prefix>_c` (crypt over it).
fn context(temp: &tempfile::TempDir, prefix: &str, daemon: bool) -> RcloneContext {
    let data = temp.path().join(prefix);
    std::fs::create_dir_all(&data).unwrap();
    let conf = temp.path().join(format!("{prefix}.conf"));
    std::fs::write(
        &conf,
        format!(
            "[{prefix}_base]\ntype = local\n\n[{prefix}_c]\ntype = crypt\nremote = {prefix}_base:{}\npassword = {}\n",
            data.display(),
            obscure("traffic test").unwrap()
        ),
    )
    .unwrap();
    let mut context = RcloneContext::new(rclone(), ConfigSelection::File(conf));
    context.environment.retain(|(k, _)| {
        let k = k.to_string_lossy().to_ascii_uppercase();
        !k.starts_with("RCLONE_") && !k.starts_with("RPOOL_RCLONE")
    });
    if daemon {
        context.allow_daemon_for_test();
    }
    context
}

#[test]
#[ignore = "requires rclone"]
fn subprocess_and_daemon_reads_and_uploads_are_counted_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let ctx = OperationContext::none();
    let plain = context(&temp, "trafficsub", false);
    let bytes = sample(100_000);
    plain
        .write_raw(
            &ctx,
            "trafficsub_c:dir/file.bin",
            &mut &bytes[..],
            Some(bytes.len() as u64),
            &WriteOptions::default(),
        )
        .unwrap();
    let after_upload = snapshot("trafficsub_c");
    assert_eq!(after_upload.sent_bytes, 100_000);
    assert_eq!(after_upload.acked_bytes, 100_000);
    assert_eq!(after_upload.failed_ops, 0);
    // Subprocess read: exactly the object's bytes.
    let mut sink = Vec::new();
    plain
        .read_raw(&ctx, "trafficsub_c:dir/file.bin", None, &mut sink)
        .unwrap();
    assert_eq!(sink, bytes);
    let after_read = snapshot("trafficsub_c");
    assert_eq!(
        after_read.received_bytes - after_upload.received_bytes,
        100_000
    );
    assert_eq!(after_read.ok_ops, after_upload.ok_ops + 1);
    // Daemon read of a range over the same config.
    let daemon = context(&temp, "trafficsub", true);
    assert!(daemon::get(&daemon).is_some(), "daemon starts");
    let range = ReadRange::new(10, 5000).unwrap();
    let mut sink = Vec::new();
    daemon
        .read_raw(&ctx, "trafficsub_c:dir/file.bin", Some(&range), &mut sink)
        .unwrap();
    assert_eq!(sink, &bytes[10..5010]);
    let after_daemon = snapshot("trafficsub_c");
    assert_eq!(
        after_daemon.received_bytes - after_read.received_bytes,
        5000
    );
    assert_eq!(after_daemon.ok_ops, after_read.ok_ops + 1);
    assert_eq!(
        (after_daemon.active_downloads, after_daemon.active_uploads),
        (0, 0)
    );
    // A missing object over the daemon is an answer, not a failure.
    daemon
        .stat_raw(&ctx, "trafficsub_c:dir/none.bin")
        .unwrap_err();
    assert_eq!(snapshot("trafficsub_c").failed_ops, 0);
    // The rates see this second's traffic one second later.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let t = snapshot("trafficsub_c");
    assert!(
        t.upload_rate_10s > 0.0 && t.download_rate_10s > 0.0,
        "{t:?}"
    );
}

#[test]
#[ignore = "requires rclone"]
fn native_pool_writes_are_credited_to_the_crypt_remote_and_monitored() {
    let temp = tempfile::tempdir().unwrap();
    let daemon = context(&temp, "trafficnat", true);
    let writer = crate::storage::writer::StorageWriter::native(daemon);
    let bytes = sample(100_000);
    writer
        .write_bytes("trafficnat_c:shards/one", &bytes, 1)
        .unwrap();
    let t = snapshot("trafficnat_c");
    // RPool's own encryption: 32-byte header + 16 bytes per 64 KiB block.
    assert_eq!(t.sent_bytes, 100_000 + 32 + 2 * 16);
    assert_eq!(t.acked_bytes, t.sent_bytes);
    assert_eq!(t.verified_bytes, 100_000);
    assert!(t.received_bytes >= 100_000, "readback counted: {t:?}");
    assert_eq!(t.failed_ops, 0);
    assert!(t.ok_ops >= 3, "{t:?}");
    assert_eq!(snapshot("trafficnat_base"), traffic::Traffic::default());
    // The monitor's status lists the pool remotes with these numbers.
    let mut sampler = Sampler::new(
        "p",
        vec!["trafficnat_c:shards".into(), "trafficidle_c:".into()],
        0,
    );
    let now = traffic::now_unix();
    let counters: Vec<_> = sampler
        .remotes()
        .iter()
        .map(|r| traffic::snapshot_at(r, now))
        .collect();
    let (status, _) = sampler.sample(now, &counters, &[], None, |_| None);
    assert_eq!(status.remotes[0].sent_bytes, t.sent_bytes);
    assert_eq!(status.remotes[0].verified_bytes, 100_000);
    assert_eq!(status.remotes[1].ok_ops, 0);
    assert!(status.alerts.is_empty());
}
