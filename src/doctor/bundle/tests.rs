//! Redaction and bundle tests with fake secrets.
use super::redact::{is_secret_key, redact_json_bytes, redact_line, redact_text};
use super::zip::tests::read_stored;
use super::*;

/// Every fake secret planted in the fixtures. None may appear in a bundle.
const SECRETS: &[&str] = &[
    "hunter2-crypt-pass",
    "S4lt-Second-Password",
    "ya29.a0AfH6SMBx9QeFakeAccessTok3n",
    "1//0gFakeRefreshTokenAbC123xyz",
    "Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MGFiY2Rl",
    "webdav-fake-bearer-77",
    "rc-user-pass-55",
    "AGE-SECRET-KEY-1QQQFAKEFAKEFAKEFAKEFAKE",
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSj",
    "client-secret-value-9",
    "alice@example.com",
    "urlpass99",
    "Basic-cred-0011",
    "s3cr3t-key-id-42",
];

fn assert_clean(text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "leaked {secret:?} in:\n{text}");
    }
}

#[test]
fn secret_key_names() {
    for key in [
        "pass",
        "password2",
        "token",
        "client_secret",
        "access_key_id",
        "secret_access_key",
        "--webdav-pass",
        "RCLONE_RC_PASS",
        "session_token",
        "Authorization",
        "private_key",
        "cookie",
    ] {
        assert!(is_secret_key(key), "{key}");
    }
    for key in [
        "type",
        "remote",
        "chunk_size",
        "workspace",
        "pool",
        "filename_encryption",
    ] {
        assert!(!is_secret_key(key), "{key}");
    }
}

#[test]
fn redacts_ini_flags_urls_and_headers() {
    let lines = [
        "password = hunter2-crypt-pass",
        "password2 = S4lt-Second-Password",
        r#"token = {"access_token":"ya29.a0AfH6SMBx9QeFakeAccessTok3n","refresh_token":"1//0gFakeRefreshTokenAbC123xyz"}"#,
        "rclone serve webdav --webdav-pass rc-user-pass-55 --addr 127.0.0.1:0",
        "RCLONE_WEBDAV_BEARER_TOKEN=webdav-fake-bearer-77",
        "GET https://bob:urlpass99@dav.example.net/path",
        "Authorization: Bearer webdav-fake-bearer-77",
        "header Basic Basic-cred-0011",
        r#""client_secret": "client-secret-value-9", "type": "drive""#,
        "access_key_id: s3cr3t-key-id-42",
        "owner alice@example.com uploaded",
        "obscured Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MGFiY2Rl here",
    ];
    for line in lines {
        assert_clean(&redact_line(line));
    }
}

#[test]
fn keeps_ordinary_diagnostic_text() {
    for line in [
        "type = crypt",
        "remote = gdrive:/data/crypt",
        "chunk_size = 64M",
        "2026/09/30 12:00:01 INFO  : vfs cache: cleaned: objects 3 (was 3) in use 0",
        "/home/user/.config/rpool/pools.json",
        "blake3 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "[gdrive_crypt]",
    ] {
        assert_eq!(redact_line(line), line);
    }
}

#[test]
fn redacts_pem_blocks_and_age_identities() {
    let text = "before\n-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSj\n-----END PRIVATE KEY-----\nAGE-SECRET-KEY-1QQQFAKEFAKEFAKEFAKEFAKE\nafter\n";
    let out = redact_text(text);
    assert_clean(&out);
    assert!(out.starts_with("before\n<redacted private key>\n"));
    assert!(out.ends_with("after\n"));
}

#[test]
fn json_secret_fields_and_embedded_strings_are_redacted() {
    let json = br#"{"version":1,"pools":{"p":{"remotes":["a:"],"password":"hunter2-crypt-pass"}},
        "nested":[{"Token":{"refresh":"1//0gFakeRefreshTokenAbC123xyz"}}],
        "note":"https://bob:urlpass99@dav.example.net/", "use_token": true, "workspace":"/w"}"#;
    let out = String::from_utf8(redact_json_bytes(json)).unwrap();
    assert_clean(&out);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["pools"]["p"]["remotes"][0], "a:");
    assert_eq!(value["use_token"], true);
    assert_eq!(value["workspace"], "/w");
}

struct FakeRclone;

impl RcloneInfo for FakeRclone {
    fn version(&self) -> anyhow::Result<String> {
        Ok("rclone v1.75.1\n- os/version: test\n".into())
    }
    /// As if rclone missed some fields: RPool's pass must still remove them.
    fn config_redacted(&self) -> anyhow::Result<String> {
        Ok("[gdrive]\ntype = drive\nclient_id = 123.apps\nclient_secret = client-secret-value-9\ntoken = {\"access_token\":\"ya29.a0AfH6SMBx9QeFakeAccessTok3n\"}\n\n[gdrive_crypt]\ntype = crypt\nremote = gdrive:/data\npassword = hunter2-crypt-pass\npassword2 = XXX\n\n[s3]\ntype = s3\naccess_key_id = s3cr3t-key-id-42\n".into())
    }
}

fn fixture_config(config: &std::path::Path, workspace: &std::path::Path) {
    std::fs::create_dir_all(config.join("mounts")).unwrap();
    std::fs::write(
        config.join("pools.json"),
        r#"{"version":1,"pools":{"main":{"remotes":["gdrive_crypt:"],"crypt_password":"hunter2-crypt-pass"}}}"#,
    )
    .unwrap();
    let gui = serde_json::json!({
        "rclone": "rclone",
        "mount_profiles": {"main": {"workspace": workspace, "mountpoint": "R:"}},
        "remotes": ["https://bob:urlpass99@dav.example.net/"],
    });
    std::fs::write(config.join("gui.json"), gui.to_string()).unwrap();
    std::fs::write(
        config.join("history.jsonl"),
        "{\"command\":\"put\",\"owner\":\"alice@example.com\"}\nnot json token=webdav-fake-bearer-77\n",
    )
    .unwrap();
    let entry = serde_json::json!({
        "id": "ab12", "pool": "main", "workspace": workspace, "mountpoint": "R:",
        "frontend": "webdav", "pid": 1, "started_unix": 1, "session_token": "rc-user-pass-55"
    });
    std::fs::write(config.join("mounts").join("ab12.json"), entry.to_string()).unwrap();
    // Not RPool's to export, even though they sit next to the config.
    std::fs::write(config.join(".env.kis"), "APP_SECRET=Basic-cred-0011\n").unwrap();
    std::fs::write(
        config.join("identity.age"),
        "AGE-SECRET-KEY-1QQQFAKEFAKEFAKEFAKEFAKE\n",
    )
    .unwrap();

    let meta = workspace.join(".rpool");
    std::fs::create_dir_all(meta.join("net-history")).unwrap();
    let mut log = String::new();
    for i in 0..2000 {
        log.push_str(&format!("2026/09/30 INFO : line {i} ok\n"));
    }
    log.push_str("2026/09/30 DEBUG : --webdav-bearer-token webdav-fake-bearer-77\n");
    log.push_str("2026/09/30 DEBUG : obscured Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MGFiY2Rl\n");
    log.push_str("-----BEGIN RSA PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSj\n-----END RSA PRIVATE KEY-----\n");
    std::fs::write(meta.join("rclone-mount.log"), log).unwrap();
    std::fs::write(
        meta.join("rclone-mount.previous.log"),
        "GET https://u:urlpass99@h/\n",
    )
    .unwrap();
    std::fs::write(
        meta.join("net-status.json"),
        r#"{"version":1,"pool":"main","remotes":[{"remote":"gdrive_crypt:","last_error":"401 for token=webdav-fake-bearer-77"}]}"#,
    )
    .unwrap();
    std::fs::write(
        meta.join("net-history").join("2026-09-29.jsonl"),
        "{\"old\":1}\n",
    )
    .unwrap();
    std::fs::write(
        meta.join("net-history").join("2026-09-30.jsonl"),
        "{\"remote\":\"gdrive_crypt:\",\"sent\":10}\n",
    )
    .unwrap();
}

#[test]
fn bundle_contains_the_expected_files_and_no_fake_secret() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let workspace = root.path().join("work");
    fixture_config(&config, &workspace);
    let output = root.path().join("out").join("diag.zip");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let doctor = vec![Diagnostic {
        check: "rclone-remotes".into(),
        status: "fail".into(),
        message: "config error: password = hunter2-crypt-pass".into(),
    }];
    let sources = Sources {
        config_dir: config,
        rclone: Some(&FakeRclone),
        doctor,
        now_unix: 1_727_740_800,
    };
    let summary = write_bundle(&sources, &output).unwrap();
    let bytes = std::fs::read(&output).unwrap();
    // Stored entries: a raw scan covers every byte of every file.
    assert_clean(&String::from_utf8_lossy(&bytes));
    let entries = read_stored(&bytes);
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    for expected in [
        "manifest.txt",
        "rpool-build.txt",
        "rclone-version.txt",
        "rclone-config-redacted.txt",
        "doctor.json",
        "config/pools.json",
        "config/gui.json",
        "config/history-tail.jsonl",
        "config/mounts/ab12.json",
        "workspaces/0/workspace.txt",
        "workspaces/0/rclone-mount.log",
        "workspaces/0/rclone-mount.previous.log",
        "workspaces/0/net-status.json",
        "workspaces/0/net-history/2026-09-30.jsonl",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    assert!(!names
        .iter()
        .any(|name| name.contains(".env") || name.contains("age")));
    assert_eq!(summary.files.len(), entries.len());
    let text = |name: &str| {
        String::from_utf8(entries.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap()
    };
    let manifest = text("manifest.txt");
    assert!(manifest.contains("config/pools.json"));
    assert!(manifest.contains("Redaction"));
    assert!(manifest.contains("rclone.conf itself"));
    let config_text = text("rclone-config-redacted.txt");
    assert!(config_text.contains("[gdrive_crypt]") && config_text.contains("type = crypt"));
    assert!(text("workspaces/0/rclone-mount.log").contains("line 1999 ok"));
    assert!(text("rpool-build.txt").contains(env!("CARGO_PKG_VERSION")));
    // The workspace was found once, through the registry and the GUI profile.
    assert!(!names.contains(&"workspaces/1/workspace.txt"));
}

#[test]
fn local_only_bundle_skips_rclone_and_mentions_it() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("diag.zip");
    let sources = Sources {
        config_dir: root.path().join("missing-config"),
        rclone: None,
        doctor: Vec::new(),
        now_unix: 0,
    };
    let summary = write_bundle(&sources, &output).unwrap();
    assert!(!summary.files.iter().any(|f| f.starts_with("rclone-")));
    let entries = read_stored(&std::fs::read(&output).unwrap());
    let manifest = String::from_utf8(entries[0].1.clone()).unwrap();
    assert!(manifest.contains("--local-only"));
    assert!(manifest.contains("config/pools.json: not present"));
}

#[test]
fn long_logs_keep_only_the_tail_from_a_line_start() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("log");
    std::fs::write(&path, "first line\nsecond line\nthird\n").unwrap();
    let (tail, total) = super::collect::read_tail(&path, 14).unwrap().unwrap();
    assert_eq!(total, 29);
    assert_eq!(tail, b"third\n");
    let (whole, _) = super::collect::read_tail(&path, 100).unwrap().unwrap();
    assert_eq!(whole.len(), 29);
    assert!(super::collect::read_tail(&root.path().join("none"), 10)
        .unwrap()
        .is_none());
}
