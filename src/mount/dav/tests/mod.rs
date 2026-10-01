//! DAV bridge tests over real loopback HTTP.

use super::*;

mod rclone_vfs;

fn request(
    server: &Server,
    method: &str,
    path: &str,
    headers: &str,
    body: &[u8],
    auth: bool,
) -> String {
    let mut socket = std::net::TcpStream::connect(server.address).unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let authorization = if auth {
        format!("Authorization: Bearer {}\r\n", server.token)
    } else {
        String::new()
    };
    write!(socket,"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n{authorization}{headers}\r\n",body.len()).unwrap();
    socket.write_all(body).unwrap();
    let mut bytes = vec![];
    socket.read_to_end(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn fragmented_write_is_one_seal_and_short_body_is_not_sealed() {
    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
    let stats = Arc::new(WriteStats::default());
    let fs = VirtualFs {
        drive: drive.clone(),
        quota_unavailable: Arc::new(AtomicBool::new(false)),
        write_stats: stats.clone(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let path = DavPath::new("/file").unwrap();
        let mut file = DavFileSystem::open(
            &fs,
            &path,
            dav_server::fs::OpenOptions {
                write: true,
                create: true,
                truncate: true,
                size: Some(64 * 4096),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        for _ in 0..64 {
            file.write_bytes(Bytes::from(vec![b'x'; 4096]))
                .await
                .unwrap();
        }
        assert!(drive.state.lock().unwrap().pending.is_empty());
        file.flush().await.unwrap();
        file.flush().await.unwrap();
        assert_eq!(drive.state.lock().unwrap().pending.len(), 1);
        assert_eq!(drive.spool_bytes().unwrap(), 64 * 4096);
        assert_eq!(stats.seals.load(Ordering::Relaxed), 1);

        let short = DavPath::new("/short").unwrap();
        let mut incomplete = DavFileSystem::open(
            &fs,
            &short,
            dav_server::fs::OpenOptions {
                write: true,
                create: true,
                truncate: true,
                size: Some(6),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        incomplete
            .write_bytes(Bytes::from_static(b"abc"))
            .await
            .unwrap();
        assert!(incomplete.flush().await.is_err());
        assert_eq!(drive.state.lock().unwrap().pending.len(), 1);
        assert_eq!(stats.incomplete.load(Ordering::Relaxed), 1);
    });
}
#[test]
fn range_put_checks_body_length_not_resulting_file_length() {
    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
    let server = Server::start(drive.clone()).unwrap();
    assert!(request(&server, "PUT", "/file", "", b"abcdef", true).starts_with("HTTP/1.1 201"));
    let response = request(
        &server,
        "PUT",
        "/file",
        "Content-Range: bytes 2-3/6\r\n",
        b"XY",
        true,
    );
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    let content = request(&server, "GET", "/file", "", b"", true);
    assert!(content.ends_with("abXYef"), "{content}");
    assert_eq!(
        server.write_stats.ranged_attempts.load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        server
            .write_stats
            .baseline_copy_bytes
            .load(Ordering::Relaxed),
        6
    );
}
#[test]
fn repeated_full_prefix_puts_have_distinct_acknowledged_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
    let server = Server::start(drive.clone()).unwrap();
    for prefix in 1..=16 {
        let body = vec![b'x'; prefix * 32768];
        let response = request(&server, "PUT", "/file", "", &body, true);
        assert!(
            response.starts_with("HTTP/1.1 201") || response.starts_with("HTTP/1.1 204"),
            "{response}"
        );
    }
    assert_eq!(drive.state.lock().unwrap().pending.len(), 16);
    assert_eq!(drive.spool_bytes().unwrap(), 32768 * (1..=16).sum::<u64>());
    assert_eq!(server.write_stats.put_successes.load(Ordering::Relaxed), 16);
    assert_eq!(server.write_stats.seals.load(Ordering::Relaxed), 16);
    assert_eq!(
        server
            .write_stats
            .baseline_copy_bytes
            .load(Ordering::Relaxed),
        0
    );
}
#[test]
fn real_http_auth_put_range_listing_and_quota() {
    let temp = tempfile::tempdir().unwrap();
    let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
    *drive.capacity.lock().unwrap() = Some(super::super::capacity::CapacityStatus {
        logical_used: 0,
        additional_estimate: 100,
        eligible: vec!["test:".into()],
        observed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        ..Default::default()
    });
    let server = Server::start(drive.clone()).unwrap();
    assert!(request(&server, "GET", "/file", "", b"", false).starts_with("HTTP/1.1 401"));
    assert!(request(&server, "PUT", "/file", "", b"hello", true).starts_with("HTTP/1.1 201"));
    assert_eq!(drive.state.lock().unwrap().pending.len(), 1);
    let response = request(&server, "GET", "/file", "Range: bytes=1-3\r\n", b"", true);
    assert!(response.starts_with("HTTP/1.1 206"), "{response}");
    assert!(response.ends_with("ell"), "{response}");
    let listing = request(&server, "PROPFIND", "/", "Depth: 1\r\n", b"", true);
    assert!(listing.starts_with("HTTP/1.1 207"), "{listing}");
    assert!(listing.contains("file"));
    let body=br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:quota-used-bytes/><d:quota-available-bytes/></d:prop></d:propfind>"#;
    let quota = request(
        &server,
        "PROPFIND",
        "/",
        "Depth: 0\r\nContent-Type: application/xml\r\n",
        body,
        true,
    );
    // Newly queued writes have not yet been reserved by the last quota sample.
    assert!(quota.contains(">5</"), "{quota}");
    assert!(quota.contains(">0</"), "{quota}");
    {
        let state = drive.state.lock().unwrap();
        let mut cached = drive.capacity.lock().unwrap();
        let c = cached.as_mut().unwrap();
        c.logical_used = 5;
        c.pending_ids = state.pending.iter().map(|i| i.id.clone()).collect();
    }
    let quota = request(
        &server,
        "PROPFIND",
        "/",
        "Depth: 0\r\nContent-Type: application/xml\r\n",
        body,
        true,
    );
    assert!(
        quota.contains(">5</") && quota.contains(">100</"),
        "{quota}"
    );
    drive
        .capacity
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .observed_unix = 0;
    let quota = request(
        &server,
        "PROPFIND",
        "/",
        "Depth: 0\r\nContent-Type: application/xml\r\n",
        body,
        true,
    );
    // Missing quota properties make rclone invent a 1 PiB volume. Stale
    // capacity must instead expose known usage and no verified free bytes.
    assert!(!quota.contains("404"), "{quota}");
    assert!(quota.contains(">5</") && quota.contains(">0</"), "{quota}");
    *drive.capacity.lock().unwrap() = None;
    let quota = request(
        &server,
        "PROPFIND",
        "/",
        "Depth: 0\r\nContent-Type: application/xml\r\n",
        body,
        true,
    );
    assert!(!quota.contains("404"), "{quota}");
    assert!(quota.contains(">5</") && quota.contains(">0</"), "{quota}");
    {
        let state = drive.state.lock().unwrap();
        *drive.capacity.lock().unwrap() = Some(super::super::capacity::CapacityStatus {
            logical_used: 5,
            additional_estimate: 200,
            eligible: vec!["test:".into()],
            observed_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            pending_ids: state.pending.iter().map(|i| i.id.clone()).collect(),
            ..Default::default()
        });
    }
    let quota = request(
        &server,
        "PROPFIND",
        "/",
        "Depth: 0\r\nContent-Type: application/xml\r\n",
        body,
        true,
    );
    assert!(
        quota.contains(">5</") && quota.contains(">200</"),
        "{quota}"
    );
    let bad = request(&server, "PUT", "/%2e%2e/outside", "", b"bad", true);
    assert!(!bad.starts_with("HTTP/1.1 201"));
}
#[test]
fn pool_sync_http_range_requests_never_mix_replaced_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = super::super::virtual_drive::fixture(temp.path());
    fixture.pool_sync_roots = vec!["crypt:pool".into()];
    fixture.state.lock().unwrap().version = 6;
    let drive = Arc::new(fixture);
    let server = Server::start(drive.clone()).unwrap();
    assert!(request(&server, "PUT", "/file", "", b"abcdef", true).starts_with("HTTP/1.1 201"));
    let first = request(&server, "GET", "/file", "Range: bytes=0-2\r\n", b"", true);
    assert!(
        first.starts_with("HTTP/1.1 206") && first.ends_with("abc"),
        "{first}"
    );
    let original = drive.view().unwrap()["file"].clone();
    let put = request(&server, "PUT", "/file", "", b"UVWXYZ", true);
    assert!(put.starts_with("HTTP/1.1 204"), "{put}");
    let second = request(&server, "GET", "/file", "Range: bytes=3-5\r\n", b"", true);
    assert!(
        !second.starts_with("HTTP/1.1 206") && !second.starts_with("HTTP/1.1 200"),
        "{second}"
    );
    assert_eq!(drive.read(&original, 3, 3).unwrap(), b"def");
}
#[test]
fn endpoint_identity_is_stable_and_occupied_port_never_changes_it() {
    let temp = tempfile::tempdir().unwrap();
    let (listener, token) = endpoint(temp.path()).unwrap();
    let address = listener.local_addr().unwrap();
    let before = fs::read(temp.path().join("dav-identity.json")).unwrap();
    assert!(endpoint(temp.path()).is_err());
    assert_eq!(
        before,
        fs::read(temp.path().join("dav-identity.json")).unwrap()
    );
    drop(listener);
    let (again, same) = endpoint(temp.path()).unwrap();
    assert_eq!(again.local_addr().unwrap(), address);
    assert_eq!(same, token);
}
#[test]
fn same_size_revisions_have_distinct_cache_metadata() {
    let a = Meta {
        size: 4,
        directory: false,
        tag: "a".into(),
    };
    let b = Meta {
        size: 4,
        directory: false,
        tag: "b".into(),
    };
    assert_ne!(a.modified().unwrap(), b.modified().unwrap());
    assert_ne!(a.etag(), b.etag());
}
