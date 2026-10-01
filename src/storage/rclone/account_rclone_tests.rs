//! Account-limit flags and the daemon bandwidth endpoint against the
//! installed rclone and a temporary local crypt remote (`RPOOL_TEST_RCLONE`
//! overrides the binary).
use super::*;
use crate::crypt::obscure::obscure;

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

fn context(temp: &tempfile::TempDir, daemon: bool) -> RcloneContext {
    let data = temp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let conf = temp.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[acct_base]\ntype = local\n\n[acct_c]\ntype = crypt\nremote = acct_base:{}\npassword = {}\n",
            data.display(),
            obscure("account test").unwrap()
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
fn daemon_takes_core_bwlimit_and_skips_unchanged_rates() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(&temp, true);
    let daemon = daemon::get(&context).expect("daemon starts");
    assert_eq!(daemon.bwlimit_rate().as_deref(), Some("off"));
    daemon.apply_bwlimit("1048576B:off");
    assert_eq!(daemon.bwlimit_rate().as_deref(), Some("1Mi:off"));
    daemon.apply_bwlimit("524288B");
    assert_eq!(daemon.bwlimit_rate().as_deref(), Some("512Ki"));
    daemon.apply_bwlimit("off");
    assert_eq!(daemon.bwlimit_rate().as_deref(), Some("off"));
    // Reads still work through the limited daemon.
    let ctx = OperationContext::none();
    context
        .write_raw(
            &ctx,
            "acct_c:f.bin",
            &mut &b"hello"[..],
            Some(5),
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(
        context.read_all_raw(&ctx, "acct_c:f.bin", None).unwrap(),
        b"hello"
    );
}

#[test]
#[ignore = "requires rclone"]
fn rclone_accepts_the_timetable_tpslimit_and_drive_upload_flags() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(&temp, false);
    let ctx = OperationContext::none();
    let args: Vec<OsString> = [
        "--bwlimit",
        "08:00,512k 18:00,10M:off 23:00,off",
        "--tpslimit",
        "2.5",
        "rcat",
        "--drive-stop-on-upload-limit",
        "--",
        "acct_c:flags.bin",
    ]
    .map(OsString::from)
    .to_vec();
    process::run(
        &mut context.base_command(&args),
        &ctx,
        Some(&mut &b"flag test"[..]),
        &mut io::sink(),
        true,
    )
    .unwrap();
    assert_eq!(
        context
            .read_all_raw(&ctx, "acct_c:flags.bin", None)
            .unwrap(),
        b"flag test"
    );
    // The account of the crypt remote is its local base, typed `local`.
    assert_eq!(
        context.write_lane(&ctx, "acct_c:x"),
        Some(("acct_base".to_string(), "local".to_string()))
    );
}

#[test]
#[ignore = "requires rclone"]
fn keepalive_records_activity_for_the_account() {
    use crate::storage::account::ledger::Ledger;
    let temp = tempfile::tempdir().unwrap();
    let context = context(&temp, false);
    let ledger = Ledger::at(temp.path().join("usage.json"));
    let outcome =
        crate::provider::keepalive::keepalive_one(&context, Some(&ledger), "acct_base:", None);
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.method, Some("about"));
    let data = ledger.load().unwrap();
    assert!(data.last_activity(["acct_base"]).is_some());
    assert!(data.accounts["acct_base"].last_keepalive.is_some());
    let missing = crate::provider::keepalive::keepalive_one(&context, Some(&ledger), "nope:", None);
    assert!(!missing.ok && missing.error.is_some());
}
