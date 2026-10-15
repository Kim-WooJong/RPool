//! Loopback DAV server lifecycle and its stable authenticated endpoint.

use super::*;

/// Requests served at once, each until its response body is sent. Further
/// requests wait for a slot. The limit is per request, not per connection:
/// rclone keeps idle keep-alive connections open for about a minute, and
/// connection slots held by idle ones left a new request unanswered while
/// rclone's NFS server queued every request of the Mac behind it.
pub(super) const REQUEST_LIMIT: usize = 32;
/// Open connections at most; one beyond it is closed at once (rclone reconnects).
const CONNECTION_LIMIT: usize = 512;

/// A response body that keeps its request slot until it is sent or dropped.
struct HeldBody {
    /// The handler's body.
    inner: dav_server::body::Body,
    /// The request slot; `None` for requests that need none (401).
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl hyper::body::Body for HeldBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// Running loopback WebDAV server of a virtual drive. rclone mounts it as `:webdav:`;
/// started by `virtual_drive::run`, stopped (and joined) on drop.
pub(crate) struct Server {
    /// Loopback listen address (stable per workspace, see [`endpoint`]).
    pub address: std::net::SocketAddr,
    /// Bearer token every request must present.
    pub token: String,
    /// Tells the accept loop to stop.
    pub(super) stop: Arc<AtomicBool>,
    /// Server thread running the tokio runtime; joined on drop.
    pub(super) thread: Option<std::thread::JoinHandle<()>>,
    /// Write counters, summarized on drop.
    pub(super) write_stats: Arc<WriteStats>,
}

impl Server {
    /// Binds the saved endpoint and serves the drive on a 2-thread tokio runtime with up to
    /// [`REQUEST_LIMIT`] requests served at once (further ones wait); unauthorized requests get 401.
    pub(crate) fn start(drive: Arc<VirtualDrive>) -> Result<Self> {
        crate::mount::adapter::preflight_virtual(&drive.root)?;
        let (listener, token) = endpoint(&drive.root)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let auth = format!("Bearer {token}");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let write_stats = Arc::new(WriteStats::default());
        let thread_stats = write_stats.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(_) => return,
                };
                let handler = DavHandler::builder()
                    .filesystem(Box::new(VirtualFs {
                        drive,
                        quota_unavailable: Arc::new(AtomicBool::new(false)),
                        write_stats: thread_stats.clone(),
                    }))
                    .locksystem(dav_server::memls::MemLs::new())
                    .build_handler();
                let requests = Arc::new(tokio::sync::Semaphore::new(REQUEST_LIMIT));
                let connections = Arc::new(tokio::sync::Semaphore::new(CONNECTION_LIMIT));
                while !flag.load(Ordering::Acquire) {
                    let accepted = tokio::time::timeout(
                        std::time::Duration::from_millis(250),
                        listener.accept(),
                    )
                    .await;
                    let Ok(Ok((stream, _))) = accepted else {
                        continue;
                    };
                    let Ok(connection) = connections.clone().try_acquire_owned() else {
                        continue;
                    };
                    let handler = handler.clone();
                    let auth = auth.clone();
                    let stats = thread_stats.clone();
                    let requests = requests.clone();
                    tokio::spawn(async move {
                        let _connection = connection;
                        let service = hyper::service::service_fn(
                            move |req: hyper::Request<hyper::body::Incoming>| {
                                let handler = handler.clone();
                                let stats = stats.clone();
                                let requests = requests.clone();
                                let allowed = req
                                    .headers()
                                    .get("authorization")
                                    .is_some_and(|h| h.as_bytes() == auth.as_bytes());
                                let is_put = allowed && req.method().as_str() == "PUT";
                                if is_put {
                                    stats.put_attempts.fetch_add(1, Ordering::Relaxed);
                                } else if allowed && req.method().as_str() == "PATCH" {
                                    stats.patch_attempts.fetch_add(1, Ordering::Relaxed);
                                }
                                if allowed && req.headers().contains_key("content-range") {
                                    stats.ranged_attempts.fetch_add(1, Ordering::Relaxed);
                                }
                                async move {
                                    let (response, permit) = if !allowed {
                                        let response = hyper::Response::builder()
                                            .status(401)
                                            .body(dav_server::body::Body::from("Unauthorized"))
                                            .unwrap();
                                        (response, None)
                                    } else {
                                        // The semaphore is never closed.
                                        let permit = requests.acquire_owned().await.ok();
                                        (handler.handle(req).await, permit)
                                    };
                                    if is_put && response.status().is_success() {
                                        stats.put_successes.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Ok::<_, std::convert::Infallible>(response.map(|inner| {
                                        HeldBody {
                                            inner,
                                            _permit: permit,
                                        }
                                    }))
                                }
                            },
                        );
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                            .await;
                    });
                }
            });
            // Abort async connections, but finish local blocking seal/fsync work
            // before releasing the workspace/process. Remote work has already
            // been cancelled by the mount lifecycle during normal shutdown.
            drop(runtime);
        });
        Ok(Self {
            address,
            token,
            stop,
            thread: Some(thread),
            write_stats,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let s = &self.write_stats;
        if s.write_opens.load(Ordering::Relaxed) > 0 {
            eprintln!(
                "RPool DAV write summary: put_attempts={} put_successes={} patch_attempts={} ranged_attempts={} write_opens={} truncating_opens={} body_bytes={} baseline_copy_bytes={} seals={} incomplete={}",
                s.put_attempts.load(Ordering::Relaxed),
                s.put_successes.load(Ordering::Relaxed),
                s.patch_attempts.load(Ordering::Relaxed),
                s.ranged_attempts.load(Ordering::Relaxed),
                s.write_opens.load(Ordering::Relaxed),
                s.truncating_opens.load(Ordering::Relaxed),
                s.body_bytes.load(Ordering::Relaxed),
                s.baseline_copy_bytes.load(Ordering::Relaxed),
                s.seals.load(Ordering::Relaxed),
                s.incomplete.load(Ordering::Relaxed),
            );
        }
    }
}

/// Loopback listener and token from `dav-identity.json`, created on first use. The port and
/// token are reused so a restarted mount reconnects to the same URL; an occupied port is an error.
pub(super) fn endpoint(root: &Path) -> Result<(std::net::TcpListener, String)> {
    #[derive(Serialize, Deserialize)]
    struct Identity {
        version: u32,
        port: u16,
        token: String,
    }
    let path = root.join("dav-identity.json");
    if path.exists() {
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("invalid DAV identity file");
        }
        let saved: Identity = crate::utils::read_json(&path)?;
        if saved.version != 1
            || saved.port == 0
            || saved.token.len() != 64
            || !saved.token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid DAV identity");
        }
        return Ok((
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, saved.port))
                .context("Saved DAV port occupied; cache identity retained, no fallback")?,
            saved.token,
        ));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let saved = Identity {
        version: 1,
        port: listener.local_addr()?.port(),
        token: crate::mount::namespace::random_id()?,
    };
    // NamedTempFile is private (0600 on Unix), and is persisted atomically.
    crate::mount::namespace::durable_json(&path, &saved)?;
    Ok((listener, saved.token))
}
