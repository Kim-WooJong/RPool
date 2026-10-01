//! Opt-in DAV tests against a real rclone VFS (ignored by default).

use super::*;

// Exercise rclone's real VFS cache without attaching the host's NFS mount.
// This test is opt-in because it needs an installed rclone and waits for
// the production write-back interval to expire.
#[test]
#[ignore = "requires rclone and the 60-second VFS write-back interval"]
fn rclone_vfs_writeback_collapses_repeated_prefix_puts() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn front_request(address: std::net::SocketAddr, method: &str, body: &[u8]) -> String {
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        write!(
            socket,
            "{method} /file HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        socket.write_all(body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        String::from_utf8(response).unwrap()
    }
    fn replay(write_back: &str, count: usize) -> (u64, u64, u64) {
        let temp = tempfile::tempdir().unwrap();
        let drive = Arc::new(crate::mount::virtual_drive::fixture(temp.path()));
        let backend = Server::start(drive.clone(), false).unwrap();
        let config = temp.path().join("empty-rclone.conf");
        std::fs::write(&config, "").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/rclone".into()
            } else {
                "rclone".into()
            }
        });
        let mut command = std::process::Command::new(rclone);
        command
            .args(["serve", "webdav", ":webdav:", "--addr"])
            .arg(address.to_string())
            .args(["--config"])
            .arg(config)
            .args(["--cache-dir"])
            .arg(temp.path().join("vfs-cache"))
            .args([
                "--vfs-cache-mode",
                "full",
                "--vfs-write-back",
                write_back,
                "--vfs-cache-poll-interval",
                "100ms",
                "--dir-cache-time",
                "1s",
            ])
            .env_clear()
            .env("RCLONE_WEBDAV_URL", format!("http://{}", backend.address))
            .env("RCLONE_WEBDAV_BEARER_TOKEN", &backend.token)
            .env("RCLONE_WEBDAV_VENDOR", "other")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = OwnedChild(command.spawn().unwrap());
        let ready_by = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if child.0.try_wait().unwrap().is_some() {
                panic!("rclone serve webdav exited before readiness");
            }
            if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(100))
                .is_ok()
            {
                break;
            }
            assert!(std::time::Instant::now() < ready_by, "rclone did not bind");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        for prefix in 1..=count {
            let body = vec![b'x'; prefix * 32768];
            let response = front_request(address, "PUT", &body);
            assert!(
                response.starts_with("HTTP/1.1 201") || response.starts_with("HTTP/1.1 204"),
                "frontend PUT {prefix} returned {}",
                response.lines().next().unwrap_or("")
            );
        }
        let expected = if write_back == "0s" { count as u64 } else { 1 };
        let complete_by = std::time::Instant::now()
            + std::time::Duration::from_secs(if write_back == "0s" { 20 } else { 85 });
        while backend.write_stats.put_successes.load(Ordering::Relaxed) < expected {
            assert!(
                std::time::Instant::now() < complete_by,
                "VFS write-back did not reach {expected} backend PUTs: got {}",
                backend.write_stats.put_successes.load(Ordering::Relaxed)
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        let view = drive.view().unwrap();
        let revision = &view["file"];
        assert_eq!(revision.size(), (count * 32768) as u64);
        assert_eq!(
            drive.read(revision, 0, revision.size() as usize).unwrap(),
            vec![b'x'; count * 32768]
        );
        (
            backend.write_stats.put_successes.load(Ordering::Relaxed),
            backend.write_stats.seals.load(Ordering::Relaxed),
            drive.spool_bytes().unwrap(),
        )
    }

    let (_, delay) = crate::mount::adapter::vfs_cache_policy(true);
    assert_eq!(delay, "60s");
    let (old_puts, old_seals, old_spool) = replay("0s", 8);
    assert!(old_puts >= 8 && old_seals >= 8);
    assert!(old_spool >= 32768 * (1..=8).sum::<u64>());
    let (new_puts, new_seals, new_spool) = replay(delay, 8);
    assert_eq!((new_puts, new_seals, new_spool), (1, 1, 8 * 32768));
}

// Real rclone VFS + RC, still without the host NFS client: the stop path's
// drain must deliver a save queued behind the 60 s write-back immediately.
#[test]
#[ignore = "requires rclone"]
fn rclone_rc_drain_delivers_delayed_writeback_immediately() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let free_port = || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(crate::mount::virtual_drive::fixture(temp.path()));
    let backend = Server::start(drive.clone(), false).unwrap();
    let config = temp.path().join("empty-rclone.conf");
    std::fs::write(&config, "").unwrap();
    let (front, rc) = (free_port(), free_port());
    let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone".into()
        } else {
            "rclone".into()
        }
    });
    let (_, write_back) = crate::mount::adapter::vfs_cache_policy(true);
    let mut command = std::process::Command::new(rclone);
    command
        .args(["serve", "webdav", ":webdav:", "--addr"])
        .arg(front.to_string())
        .args(["--rc", "--rc-addr"])
        .arg(rc.to_string())
        .arg("--config")
        .arg(config)
        .arg("--cache-dir")
        .arg(temp.path().join("vfs-cache"))
        .args(["--vfs-cache-mode", "full", "--vfs-write-back", write_back])
        .env_clear()
        .env("RCLONE_RC_USER", "rpool")
        .env("RCLONE_RC_PASS", "password")
        .env("RCLONE_WEBDAV_URL", format!("http://{}", backend.address))
        .env("RCLONE_WEBDAV_BEARER_TOKEN", &backend.token)
        .env("RCLONE_WEBDAV_VENDOR", "other")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = OwnedChild(command.spawn().unwrap());
    let ready_by = std::time::Instant::now() + std::time::Duration::from_secs(15);
    for address in [front, rc] {
        while std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(100))
            .is_err()
        {
            assert!(child.0.try_wait().unwrap().is_none(), "rclone exited early");
            assert!(std::time::Instant::now() < ready_by, "rclone did not bind");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let body = vec![b'y'; 65536];
    let mut socket = std::net::TcpStream::connect(front).unwrap();
    write!(
        socket,
        "PUT /saved HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    socket.write_all(&body).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert_eq!(backend.write_stats.put_successes.load(Ordering::Relaxed), 0);
    let started = std::time::Instant::now();
    let drained = crate::mount::adapter::drain_writeback(
        rc,
        "cnBvb2w6cGFzc3dvcmQ=",
        std::time::Duration::from_secs(40),
    )
    .unwrap();
    assert!(drained);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(backend.write_stats.put_successes.load(Ordering::Relaxed), 1);
    let view = drive.view().unwrap();
    let revision = &view["saved"];
    assert_eq!(drive.read(revision, 0, 65536).unwrap(), body);
}

#[test]
#[ignore = "requires rclone"]
fn rclone_about_reports_mount_capacity_for_default_64_mib_pool() {
    use crate::mount::capacity::CapacityStatus;
    let gib = 1u64 << 30;
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    // Seven independent accounts, Resilient RS 9+3, default (64 MiB) shards.
    let policy = crate::models::PoolDefinition {
        remotes: (0..7).map(|i| format!("p{i}:")).collect(),
        data_shards: 9,
        parity_shards: 3,
        placement: crate::models::Placement::Resilient,
        ..crate::models::PoolDefinition::default()
    };
    assert_eq!(policy.shard_bytes().unwrap().get(), 64 << 20);
    let frees = [15u64, 10, 20, 5, 50, 12, 8];
    let mut status = CapacityStatus {
        targets: frees
            .iter()
            .enumerate()
            .map(|(i, free)| crate::storage::admin::budget::TargetBudget {
                remote: format!("p{i}:"),
                backing: format!("p{i}"),
                capacity_domain: format!("p{i}"),
                failure_domain: Some(format!("p{i}")),
                declared: true,
                total: 2 * free * gib,
                free: free * gib,
            })
            .collect(),
        ..Default::default()
    };
    status.eligible = status.targets.iter().map(|t| t.remote.clone()).collect();
    status.budget = frees.iter().sum::<u64>() * gib;
    status.recalculate(&policy).unwrap();
    let estimate = status.additional_estimate;
    assert!(estimate > 69 * gib, "{estimate}");
    status.check_upload(&policy, estimate).unwrap();
    status.observed_unix = now();

    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(crate::mount::virtual_drive::fixture(temp.path()));
    *drive.capacity.lock().unwrap() = Some(status);
    let server = Server::start(drive.clone(), false).unwrap();
    let config = temp.path().join("empty-rclone.conf");
    std::fs::write(&config, "").unwrap();
    let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone".into()
        } else {
            "rclone".into()
        }
    });
    // Same :webdav: wiring as the native mount adapter; its statfs comes from About.
    let about = || -> serde_json::Value {
        let output = std::process::Command::new(&rclone)
            .args(["about", ":webdav:", "--json", "--config"])
            .arg(&config)
            .env_clear()
            .env("RCLONE_WEBDAV_URL", format!("http://{}/", server.address))
            .env("RCLONE_WEBDAV_BEARER_TOKEN", &server.token)
            .env("RCLONE_WEBDAV_VENDOR", "other")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };

    let empty = about();
    assert_eq!(empty["used"], 0, "{empty}");
    assert_eq!(empty["free"], estimate, "{empty}");
    assert_eq!(empty["total"], estimate, "{empty}");

    assert!(request(&server, "PUT", "/file", "", b"hello", true).starts_with("HTTP/1.1 201"));
    {
        // Mirror a completed capacity refresh that already accounts for the write.
        let state = drive.state.lock().unwrap();
        let mut cached = drive.capacity.lock().unwrap();
        let c = cached.as_mut().unwrap();
        c.logical_used = 5;
        c.pending_ids = state.pending.iter().map(|i| i.id.clone()).collect();
    }
    let written = about();
    assert_eq!(written["used"], 5, "{written}");
    assert_eq!(written["free"], estimate, "{written}");
    assert_eq!(written["total"], estimate + 5, "{written}");

    drive
        .capacity
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .observed_unix = now() - 121;
    let stale = about();
    assert_eq!(stale["used"], 5, "{stale}");
    assert_eq!(stale["free"], 0, "{stale}");
}
