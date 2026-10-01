//! Loopback DAV server lifecycle and its stable authenticated endpoint.

use super::*;

pub(crate) struct Server {
    pub address: std::net::SocketAddr,
    pub token: String,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) thread: Option<std::thread::JoinHandle<()>>,
    pub(super) write_stats: Arc<WriteStats>,
}

impl Server {
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
                let limit = Arc::new(tokio::sync::Semaphore::new(32));
                while !flag.load(Ordering::Acquire) {
                    let accepted = tokio::time::timeout(
                        std::time::Duration::from_millis(250),
                        listener.accept(),
                    )
                    .await;
                    let Ok(Ok((stream, _))) = accepted else {
                        continue;
                    };
                    let Ok(permit) = limit.clone().try_acquire_owned() else {
                        continue;
                    };
                    let handler = handler.clone();
                    let auth = auth.clone();
                    let stats = thread_stats.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = hyper::service::service_fn(
                            move |req: hyper::Request<hyper::body::Incoming>| {
                                let handler = handler.clone();
                                let stats = stats.clone();
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
                                    let response = if !allowed {
                                        hyper::Response::builder()
                                            .status(401)
                                            .body(dav_server::body::Body::from("Unauthorized"))
                                            .unwrap()
                                    } else {
                                        handler.handle(req).await
                                    };
                                    if is_put && response.status().is_success() {
                                        stats.put_successes.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Ok::<_, std::convert::Infallible>(response)
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
