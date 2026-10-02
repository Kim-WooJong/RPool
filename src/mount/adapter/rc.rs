//! rclone remote-control calls and the write-back drain before quitting.

use super::*;

/// POSTs `body` to rclone's RC `endpoint` over HTTP/1.0 with Basic auth and returns the
/// response body; non-200 is an error. Reads at most `limit` bytes.
pub(super) fn rc_call(
    address: SocketAddr,
    credential: &str,
    endpoint: &str,
    body: &str,
    timeout: Duration,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(stream, "POST /{endpoint} HTTP/1.0\r\nHost: {address}\r\nAuthorization: Basic {credential}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    let mut response = Vec::new();
    stream.take(limit).read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid mount control response")?;
    let headers = std::str::from_utf8(&response[..split])?;
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"))
    {
        bail!("mount control endpoint unavailable");
    }
    Ok(response[split + 4..].to_vec())
}

/// Makes rclone upload its delayed write-back queue now and waits until it is
/// empty. Returns false when `limit` expires. Items already retrying after an
/// upload error keep their backoff instead of being retried in a tight loop.
pub(in crate::mount) fn drain_writeback(
    address: SocketAddr,
    credential: &str,
    limit: Duration,
) -> Result<bool> {
    let started = Instant::now();
    let mut next_note = started + Duration::from_secs(10);
    loop {
        let stats = rc_call(
            address,
            credential,
            "vfs/stats",
            "{}",
            Duration::from_secs(2),
            1024 * 1024,
        )?;
        let stats: serde_json::Value =
            serde_json::from_slice(&stats).context("invalid rclone VFS statistics")?;
        let Some(disk) = stats.get("diskCache") else {
            return Ok(true); // No VFS cache, so nothing can be queued.
        };
        let pending = disk["uploadsQueued"].as_u64().unwrap_or(0)
            + disk["uploadsInProgress"].as_u64().unwrap_or(0);
        if pending == 0 {
            return Ok(true);
        }
        if started.elapsed() >= limit {
            return Ok(false);
        }
        let queue = rc_call(
            address,
            credential,
            "vfs/queue",
            "{}",
            Duration::from_secs(2),
            8 * 1024 * 1024,
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        for item in queue
            .as_ref()
            .and_then(|queue| queue["queue"].as_array())
            .into_iter()
            .flatten()
        {
            let waiting = item["uploading"].as_bool() == Some(false)
                && item["tries"].as_u64().unwrap_or(0) == 0
                && item["expiry"].as_f64().is_some_and(|expiry| expiry > 0.0);
            if let (true, Some(id)) = (waiting, item["id"].as_i64()) {
                // An item that started uploading meanwhile just returns an error.
                let _ = rc_call(
                    address,
                    credential,
                    "vfs/queue-set-expiry",
                    &format!("{{\"id\":{id},\"expiry\":-1000000000}}"),
                    Duration::from_secs(2),
                    1024 * 1024,
                );
            }
        }
        if Instant::now() >= next_note {
            eprintln!(
                "Waiting for {pending} native saves to reach the RPool spool before unmounting"
            );
            next_note += Duration::from_secs(10);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Standard padded base64 encoding (used for the RC Basic-auth header).
pub(super) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
