//! Mount process, lease and log tests.

use super::*;
use std::sync::Arc;
#[test]
fn dav_vfs_policy_keeps_dirty_writes_until_after_the_nfs_callback_burst() {
    assert_eq!(vfs_cache_policy(true), ("full", "60s"));
    assert_eq!(vfs_cache_policy(false), ("writes", "0s"));
}

fn lease_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    PathBuf,
) {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let files = root.path().join("files");
    let cache = root.path().join("cache");
    std::fs::create_dir(&files).unwrap();
    std::fs::create_dir(&cache).unwrap();
    std::fs::create_dir(root.path().join(".rpool")).unwrap();
    #[cfg(windows)]
    let target = PathBuf::from("R:");
    #[cfg(not(windows))]
    let target = other.path().canonicalize().unwrap();
    (
        root,
        other,
        files.canonicalize().unwrap(),
        cache.canonicalize().unwrap(),
        target,
    )
}

#[test]
fn lease_survives_owner_drop_and_uncertain_launch_blocks_restart() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    lease.record(&LeaseState::Launching).unwrap();
    let path = lease.path.clone();
    drop(lease);
    assert!(path.exists());
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
}

#[test]
fn active_pid_blocks_restart_without_sending_signal() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    lease
        .record(&LeaseState::Running {
            pid: std::process::id(),
        })
        .unwrap();
    drop(lease);
    assert!(process_alive(std::process::id()).unwrap());
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    assert!(process_alive(0).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn uncertain_nfs_shutdown_never_kills_child_or_clears_lease() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    let child = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let pid = child.id();
    lease.record(&LeaseState::Running { pid }).unwrap();
    let lease_path = lease.path.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut process = MountProcess {
        child,
        target: target.clone(),
        address,
        credential: "synthetic".into(),
        logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
        stopped: false,
        graceful_quit_requested: false,
        shutdown_uncertain: false,
        lease,
    };
    assert!(process.stop_with_grace(Duration::from_millis(1)).is_err());
    assert!(process.shutdown_uncertain);
    assert!(process.child.try_wait().unwrap().is_none());
    drop(process);
    assert!(process_alive(pid).unwrap());
    assert!(lease_path.exists());
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
        libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
    }
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn unexpected_nfs_child_exit_retains_lease_even_without_mount_table_entry() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    let child = Command::new("/usr/bin/true").spawn().unwrap();
    lease
        .record(&LeaseState::Running { pid: child.id() })
        .unwrap();
    let path = lease.path.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut process = MountProcess {
        child,
        target: target.clone(),
        address,
        credential: "synthetic".into(),
        logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
        stopped: false,
        graceful_quit_requested: false,
        shutdown_uncertain: false,
        lease,
    };
    process.child.wait().unwrap();
    assert!(process.poll().is_err());
    drop(process);
    assert!(path.exists());
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn nfsmount_minimum_version_is_reported() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("old-rclone");
    std::fs::write(&script, "#!/bin/sh\necho 'rclone v1.64.0'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(check_nfsmount_version(script.to_str().unwrap()).is_err());
    std::fs::write(&script, "#!/bin/sh\necho 'rclone v1.65.0'\n").unwrap();
    assert!(check_nfsmount_version(script.to_str().unwrap()).is_ok());
}

#[test]
fn dropping_lease_unlocks_even_with_an_inherited_description() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    // Deterministically model the descriptor inherited across fork without
    // depending on thread/process scheduling or weakening exclusive locking.
    let inherited = lease._lock.try_clone().unwrap();
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    drop(lease);
    let next = MountLease::prepare(&files, &cache, &target, None).unwrap();
    assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    drop(next);
    drop(inherited);
}

#[test]
fn identity_is_stable_after_clean_stop() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    lease.record(&LeaseState::Launching).unwrap();
    lease.clear().unwrap();
    drop(lease);
    drop(MountLease::prepare(&files, &cache, &target, None).unwrap());
    let different_cache = cache.parent().unwrap().join("other-cache");
    std::fs::create_dir(&different_cache).unwrap();
    assert!(MountLease::prepare(&files, &different_cache, &target, None).is_err());
    #[cfg(windows)]
    let different_target = PathBuf::from("S:");
    #[cfg(not(windows))]
    let different_target = tempfile::tempdir().unwrap();
    #[cfg(not(windows))]
    let different_target_path = different_target.path();
    #[cfg(windows)]
    let different_target_path = different_target.as_path();
    assert!(MountLease::prepare(&files, &cache, different_target_path, None).is_err());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn forced_stop_reaps_owned_child_and_preserves_cache() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let sentinel = cache.join("pending-write");
    std::fs::write(&sentinel, b"must survive").unwrap();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    lease.record(&LeaseState::Launching).unwrap();
    // Synthetic local child only: no rclone, driver, cloud or configuration access.
    let child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 60"])
        .spawn()
        .unwrap();
    let pid = child.id();
    lease.record(&LeaseState::Running { pid }).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let lease_path = lease.path.clone();
    let mut process = MountProcess {
        child,
        target,
        address,
        credential: "synthetic".into(),
        logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
        stopped: false,
        graceful_quit_requested: false,
        shutdown_uncertain: false,
        lease,
    };
    let report = process.stop().unwrap();
    assert!(report.forced);
    assert!(report.cache_preserved);
    assert!(process.child.try_wait().unwrap().is_some());
    assert!(!lease_path.exists());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"must survive");
}

#[test]
fn mount_log_tails_incrementally_redacts_and_bounds_output() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rclone-mount.log");
    let mut log = MountLog::new(path.clone(), vec!["s3cret".into(), String::new()]);
    assert!(log.read_new().is_empty()); // Missing file is not an error.
    std::fs::write(&path, b"first s3cret line\npartial").unwrap();
    assert_eq!(log.read_new(), ["first [redacted] line"]);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b" continued\n").unwrap();
    assert_eq!(log.read_new(), ["partial continued"]);
    file.write_all(&vec![b'x'; LOG_LINE_LIMIT + 10]).unwrap();
    assert!(log.read_new().is_empty());
    file.write_all(b"tail-s3cret\nnext\n").unwrap();
    assert_eq!(
        log.read_new(),
        ["[oversized mount log line omitted]", "next"]
    );
    let many: String = (0..LOG_LINES_RETURNED + 5)
        .map(|i| format!("{i}\n"))
        .collect();
    file.write_all(many.as_bytes()).unwrap();
    let lines = log.read_new();
    assert_eq!(lines.len(), LOG_LINES_RETURNED + 1);
    assert_eq!(lines[0], "[5 earlier mount log lines omitted]");
    assert_eq!(lines.last().unwrap(), &(LOG_LINES_RETURNED + 4).to_string());
}

#[test]
fn mount_log_is_private_and_keeps_previous_session() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rclone-mount.log");
    std::fs::write(&path, b"old session\n").unwrap();
    let mut file = open_mount_log(&path).unwrap();
    file.write_all(b"new session\n").unwrap();
    assert_eq!(
        std::fs::read(root.path().join("rclone-mount.previous.log")).unwrap(),
        b"old session\n"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new session\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let link = root.path().join("linked.log");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(open_mount_log(&link).is_err());
    }
}

/// Minimal authenticated rclone RC stand-in: answers vfs/stats, vfs/queue and
/// records vfs/queue-set-expiry calls.
fn fake_rc(
    queued: Arc<std::sync::atomic::AtomicU64>,
    tries: u64,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    use std::sync::atomic::Ordering;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(&buffer[..n]);
                if n == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let head = text.split("\r\n\r\n").next().unwrap_or("").to_string();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let mut body = text
                .split_once("\r\n\r\n")
                .map(|(_, b)| b.to_string())
                .unwrap_or_default();
            while body.len() < length {
                let n = stream.read(&mut buffer).unwrap_or(0);
                if n == 0 {
                    break;
                }
                body.push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
            let authorized = head.contains("Authorization: Basic cnBvb2w6cGFzc3dvcmQ=");
            let endpoint = head.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().unwrap().push(format!("{endpoint} {body}"));
            let reply = match (authorized, endpoint.as_str()) {
                (false, _) => None,
                (true, "/vfs/stats") => Some(format!(
                    "{{\"diskCache\":{{\"uploadsQueued\":{},\"uploadsInProgress\":0}}}}",
                    queued.load(Ordering::SeqCst)
                )),
                (true, "/vfs/queue") => Some(format!(
                    "{{\"queue\":[{{\"id\":7,\"uploading\":false,\"tries\":{tries},\"expiry\":59.5}}]}}"
                )),
                (true, "/vfs/queue-set-expiry") => {
                    queued.store(0, Ordering::SeqCst);
                    Some("{}".into())
                }
                _ => None,
            };
            let response = match reply {
                Some(json) => format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{json}",
                    json.len()
                ),
                None => "HTTP/1.0 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (address, calls)
}

#[test]
fn drain_expedites_delayed_writeback_until_queue_is_empty() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let credential = base64(b"rpool:password");
    let queued = Arc::new(AtomicU64::new(1));
    let (address, calls) = fake_rc(queued.clone(), 0);
    assert!(drain_writeback(address, &credential, Duration::from_secs(10)).unwrap());
    assert_eq!(queued.load(Ordering::SeqCst), 0);
    let calls = calls.lock().unwrap();
    assert!(calls
        .iter()
        .any(|c| c == "/vfs/queue-set-expiry {\"id\":7,\"expiry\":-1000000000}"));
}

#[test]
fn drain_respects_retry_backoff_and_time_limit() {
    use std::sync::atomic::AtomicU64;
    let credential = base64(b"rpool:password");
    let (address, calls) = fake_rc(Arc::new(AtomicU64::new(3)), 2);
    let started = Instant::now();
    assert!(!drain_writeback(address, &credential, Duration::from_millis(300)).unwrap());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.starts_with("/vfs/queue-set-expiry")));
    // Wrong credentials or no RC server are reported, not treated as drained.
    assert!(drain_writeback(address, "wrong", Duration::from_secs(1)).is_err());
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    assert!(drain_writeback(closed, &credential, Duration::from_secs(1)).is_err());
}

#[cfg(unix)]
#[test]
fn late_graceful_exit_is_awaited_without_killing_and_then_clears_lease() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 0.5"])
        .spawn()
        .unwrap();
    lease
        .record(&LeaseState::ShutdownUncertain { pid: child.id() })
        .unwrap();
    let lease_path = lease.path.clone();
    let mut process = MountProcess {
        child,
        target,
        address: TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap(),
        credential: "synthetic".into(),
        logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
        stopped: false,
        graceful_quit_requested: true,
        shutdown_uncertain: true,
        lease,
    };
    assert!(process.is_running());
    assert!(!process.wait_for_exit(Duration::from_millis(50)).unwrap());
    assert!(process.is_running(), "waiting must never kill the child");
    assert!(process.wait_for_exit(Duration::from_secs(10)).unwrap());
    assert!(!process.is_running());
    assert!(!process.shutdown_uncertain);
    assert!(!lease_path.exists());
}

#[cfg(unix)]
#[test]
fn late_exit_without_graceful_quit_keeps_uncertain_lease() {
    let (_root, _other, files, cache, target) = lease_fixture();
    let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    lease
        .record(&LeaseState::ShutdownUncertain { pid: child.id() })
        .unwrap();
    let lease_path = lease.path.clone();
    let mut process = MountProcess {
        child,
        target,
        address: TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap(),
        credential: "synthetic".into(),
        logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
        stopped: false,
        graceful_quit_requested: false,
        shutdown_uncertain: true,
        lease,
    };
    assert!(process.wait_for_exit(Duration::from_secs(10)).unwrap());
    assert!(process.shutdown_uncertain);
    assert!(lease_path.exists());
    drop(process);
    assert!(lease_path.exists());
}

#[cfg(unix)]
#[test]
fn rclone_output_goes_to_private_log_file_not_pipes() {
    use std::os::unix::fs::PermissionsExt;
    let (root, _other, files, cache, target) = lease_fixture();
    let script = root.path().join("fake rclone");
    std::fs::write(
        &script,
        "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'rclone v1.75.1'; exit 0; fi\n\
         echo \"stdout pass=$RCLONE_RC_PASS\"\n\
         for a in \"$@\"; do [ \"$prev\" = --volname ] && echo \"volname=$a\"; prev=\"$a\"; done\n\
         echo \"stderr token=$RCLONE_WEBDAV_BEARER_TOKEN\" >&2\n\
         [ -p /dev/stdout ] && echo 'stdout is a pipe'\n\
         exec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = MountProcess::start(MountConfig {
        rclone: script.to_str().unwrap().into(),
        files_dir: files,
        cache_dir: cache,
        target,
        shared: false,
        read_only: false,
        vfs_cache_gib: 1,
        cache_min_free_gib: 0,
        webdav: Some(("http://127.0.0.1:9/".into(), "bearer-secret-token".into())),
        volume_name: Some("My Pool".into()),
    })
    .unwrap();
    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while lines.len() < 3 && Instant::now() < deadline {
        lines.extend(process.logs());
        thread::sleep(Duration::from_millis(50));
    }
    let log_path = root.path().join(".rpool/rclone-mount.log");
    let mode = std::fs::metadata(&log_path).unwrap().permissions().mode();
    process.child.kill().unwrap();
    process.child.wait().unwrap();
    process.stopped = true;
    drop(process);
    assert_eq!(mode & 0o777, 0o600);
    assert!(
        lines.contains(&"stdout pass=[redacted]".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"stderr token=[redacted]".to_string()),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("pipe")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("bearer-secret-token")));
    assert!(lines.contains(&"volname=My Pool".to_string()), "{lines:?}");
}

#[test]
fn volume_labels_are_the_pool_name_made_safe() {
    assert_eq!(volume_label("archive"), "archive");
    assert_eq!(volume_label("My Pool"), "My Pool");
    assert_eq!(volume_label("a,b:c/d\\e\"f"), "abcdef");
    assert_eq!(volume_label(&"x".repeat(40)).len(), 32);
    assert_eq!(volume_label("사진 보관"), "사진 보관");
    assert_eq!(volume_label(",,,"), "RPool");
}

#[test]
fn encodes_basic_auth() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"rpool:password"), "cnBvb2w6cGFzc3dvcmQ=");
}
#[cfg(unix)]
#[test]
fn rejects_overlap_nonempty_and_symlink_targets() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let files = root.join("files");
    let target_root = tempfile::tempdir().unwrap();
    let target = target_root.path().canonicalize().unwrap().join("target");
    std::fs::create_dir(&files).unwrap();
    std::fs::create_dir(&target).unwrap();
    let mut config = MountConfig {
        rclone: "rclone".into(),
        files_dir: files.clone(),
        cache_dir: root.join("cache"),
        target: target.clone(),
        shared: false,
        read_only: false,
        vfs_cache_gib: 10,
        cache_min_free_gib: 2,
        webdav: None,
        volume_name: None,
    };
    assert!(validate_mountpoint(&config).is_ok());
    let metadata = root.join(".rpool/archives");
    std::fs::create_dir_all(&metadata).unwrap();
    config.target = metadata;
    assert!(validate_mountpoint(&config).is_err());
    config.target = target.clone();
    config.cache_dir = files.join("cache");
    assert!(validate_mountpoint(&config).is_err());
    config.cache_dir = root.join("cache");
    std::fs::write(target.join("occupied"), b"x").unwrap();
    assert!(validate_mountpoint(&config).is_err());
    let link = root.join("link");
    std::os::unix::fs::symlink(&files, &link).unwrap();
    config.target = link;
    assert!(validate_mountpoint(&config).is_err());
}
