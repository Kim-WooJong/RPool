//! Loopback-only authenticated bridge. DAV handles pin immutable revisions.
use super::{
    namespace::Intent,
    virtual_drive::{Revision, VirtualDrive},
};
use crate::prelude::*;
use bytes::{Buf, Bytes};
use dav_server::{davpath::DavPath, fs::*, DavHandler};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
struct WriteStats {
    put_attempts: AtomicU64,
    put_successes: AtomicU64,
    patch_attempts: AtomicU64,
    ranged_attempts: AtomicU64,
    write_opens: AtomicU64,
    truncating_opens: AtomicU64,
    body_bytes: AtomicU64,
    baseline_copy_bytes: AtomicU64,
    seals: AtomicU64,
    incomplete: AtomicU64,
}

fn failure(e: impl std::fmt::Display) -> FsError {
    eprintln!("Virtual filesystem: {e}");
    FsError::GeneralFailure
}
fn path(p: &DavPath) -> FsResult<String> {
    let raw = p
        .as_rel_ospath()
        .to_str()
        .ok_or(FsError::Forbidden)?
        .trim_end_matches('/');
    if !raw.is_empty() {
        super::namespace::valid_path(raw).map_err(|_| FsError::Forbidden)?;
    }
    Ok(raw.into())
}
#[derive(Clone, Debug)]
struct Meta {
    size: u64,
    directory: bool,
    tag: String,
}
impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.size
    }
    fn is_dir(&self) -> bool {
        self.directory
    }
    fn modified(&self) -> FsResult<SystemTime> {
        // Stable revision-derived timestamp: same-size replacement must invalidate
        // clients that compare only size/mtime instead of DAV ETags.
        let hash = blake3::hash(self.tag.as_bytes());
        let n = u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap());
        Ok(UNIX_EPOCH + std::time::Duration::from_secs(946684800 + n % 1893456000))
    }
    fn etag(&self) -> Option<String> {
        Some(self.tag.clone())
    }
}
struct Entry {
    name: String,
    meta: Meta,
}
impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move { Ok(Box::new(self.meta.clone()) as _) })
    }
}
#[derive(Clone)]
struct VirtualFs {
    drive: Arc<VirtualDrive>,
    quota_unavailable: Arc<AtomicBool>,
    write_stats: Arc<WriteStats>,
}
impl VirtualFs {
    fn stat(&self, path: &str) -> FsResult<Meta> {
        let view = self.drive.view().map_err(failure)?;
        if let Some(r) = view.get(path) {
            return Ok(Meta {
                size: r.size(),
                directory: false,
                tag: r.id().into(),
            });
        }
        if path.is_empty()
            || view.keys().any(|p| p.starts_with(&format!("{path}/")))
            || self.drive.state.lock().unwrap().directories.contains(path)
        {
            return Ok(Meta {
                size: 0,
                directory: true,
                tag: format!("dir-{}", self.drive.state.lock().unwrap().generation),
            });
        }
        Err(FsError::NotFound)
    }
}
impl DavFileSystem for VirtualFs {
    fn metadata<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move { Ok(Box::new(self.stat(&path(p)?)?) as _) })
    }
    fn read_dir<'a>(
        &'a self,
        p: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let p = path(p)?;
            if !self.stat(&p)?.directory {
                return Err(FsError::Forbidden);
            }
            let prefix = if p.is_empty() {
                String::new()
            } else {
                format!("{p}/")
            };
            let mut entries = BTreeMap::new();
            for (name, r) in self.drive.view().map_err(failure)? {
                if let Some(relative) = name.strip_prefix(&prefix) {
                    if let Some((child, _)) = relative.split_once('/') {
                        entries.insert(
                            child.to_owned(),
                            Meta {
                                size: 0,
                                directory: true,
                                tag: format!("dir-{child}"),
                            },
                        );
                    } else {
                        entries.insert(
                            relative.to_owned(),
                            Meta {
                                size: r.size(),
                                directory: false,
                                tag: r.id().into(),
                            },
                        );
                    }
                }
            }
            for name in &self.drive.state.lock().unwrap().directories {
                if let Some(relative) = name.strip_prefix(&prefix) {
                    let child = relative.split('/').next().unwrap_or("");
                    if !child.is_empty() {
                        entries.entry(child.into()).or_insert(Meta {
                            size: 0,
                            directory: true,
                            tag: format!("dir-{child}"),
                        });
                    }
                }
            }
            let entries: Vec<FsResult<Box<dyn DavDirEntry>>> = entries
                .into_iter()
                .map(|(name, meta)| Ok(Box::new(Entry { name, meta }) as _))
                .collect();
            Ok(Box::pin(futures_util::stream::iter(entries)) as _)
        })
    }
    fn open<'a>(
        &'a self,
        p: &'a DavPath,
        options: dav_server::fs::OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let p = path(p)?;
            if p.is_empty() {
                return Err(FsError::Forbidden);
            }
            let drive = self.drive.clone();
            let stats = self.write_stats.clone();
            tokio::task::spawn_blocking(move || {
                let revision = drive.view().map_err(failure)?.get(&p).cloned();
                if options.create_new && revision.is_some() {
                    return Err(FsError::Exists);
                }
                if options.write {
                    stats.write_opens.fetch_add(1, Ordering::Relaxed);
                    if options.truncate {
                        stats.truncating_opens.fetch_add(1, Ordering::Relaxed);
                    }
                    if !options.create && revision.is_none() {
                        return Err(FsError::NotFound);
                    }
                    let intent = drive
                        .begin_observed(&p, revision.as_ref())
                        .map_err(failure)?;
                    let mut file = std::fs::OpenOptions::new()
                        .create_new(true)
                        .read(true)
                        .write(true)
                        .open(drive.spool_path(&intent))
                        .map_err(failure)?;
                    if !options.truncate {
                        if let Some(r) = &revision {
                            let mut offset = 0;
                            while offset < r.size() {
                                let bytes = drive.read(r, offset, 1024 * 1024).map_err(failure)?;
                                drive
                                    .write_spool_bytes(&mut file, &bytes)
                                    .map_err(failure)?;
                                stats
                                    .baseline_copy_bytes
                                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                                offset += bytes.len() as u64;
                            }
                            file.seek(SeekFrom::Start(0)).map_err(failure)?;
                        }
                    }
                    if options.append {
                        file.seek(SeekFrom::End(0)).map_err(failure)?;
                    }
                    let write_lease = drive.local_lease(&intent.id);
                    Ok(Box::new(Handle {
                        _write_lease: Some(write_lease),
                        drive,
                        revision,
                        intent: Some(intent),
                        file: Some(file),
                        offset: 0,
                        expected: options.size,
                        received: 0,
                        sealed: false,
                        write_stats: stats,
                    }) as Box<dyn DavFile>)
                } else {
                    let revision = revision.ok_or(FsError::NotFound)?;
                    // Persist ancestry when opening, never on a metadata listing.
                    drive.pin_read(&p, &revision).map_err(failure)?;
                    Ok(Box::new(Handle {
                        _write_lease: None,
                        drive,
                        revision: Some(revision),
                        intent: None,
                        file: None,
                        offset: 0,
                        expected: None,
                        received: 0,
                        sealed: false,
                        write_stats: stats,
                    }) as Box<dyn DavFile>)
                }
            })
            .await
            .map_err(failure)?
        })
    }
    fn create_dir<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let p = path(p)?;
            if self.stat(&p).is_ok() {
                return Err(FsError::Exists);
            }
            let mut s = self.drive.state.lock().unwrap();
            let mut next = s.clone();
            next.directories.insert(p);
            next.save(&self.drive.root).map_err(failure)?;
            *s = next;
            Ok(())
        })
    }
    fn remove_file<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let p = path(p)?;
            if self.stat(&p)?.directory {
                return Err(FsError::Forbidden);
            }
            self.drive.delete(&p).map_err(failure)
        })
    }
    fn remove_dir<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let p = path(p)?;
            if p.is_empty() {
                return Err(FsError::Forbidden);
            }
            if self
                .drive
                .view()
                .map_err(failure)?
                .keys()
                .any(|n| n.starts_with(&format!("{p}/")))
            {
                return Err(FsError::Exists);
            }
            let mut s = self.drive.state.lock().unwrap();
            if s.directories
                .iter()
                .any(|name| name.starts_with(&format!("{p}/")))
            {
                return Err(FsError::Exists);
            }
            let mut next = s.clone();
            next.directories.remove(&p);
            next.save(&self.drive.root).map_err(failure)?;
            *s = next;
            Ok(())
        })
    }
    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let from = path(from)?;
            let to = path(to)?;
            let drive = self.drive.clone();
            let directory = self.stat(&from)?.directory;
            tokio::task::spawn_blocking(move || {
                if directory {
                    drive.rename_directory(&from, &to)
                } else {
                    drive.rename_file(&from, &to)
                }
                .map_err(failure)
            })
            .await
            .map_err(failure)?
        })
    }
    fn get_quota(&self) -> FsFuture<'_, (u64, Option<u64>)> {
        Box::pin(async move {
            if let Some(quota) = self.drive.quota() {
                if self.quota_unavailable.swap(false, Ordering::AcqRel) {
                    eprintln!("Virtual drive capacity verified again; Explorer now shows the current writable-space estimate.");
                }
                return Ok(quota);
            }
            // rclone's WebDAV About reads these RFC quota properties. Missing
            // properties become its synthetic 1 PiB Statfs fallback. Advertise
            // only confirmed namespace usage and zero *additional* writable
            // bytes while cloud capacity is unverified, not an invented quota.
            let used = self
                .drive
                .state
                .lock()
                .map_err(|_| failure("namespace lock poisoned while reading quota"))?
                .visible_logical_used()
                .map_err(failure)?;
            if !self.quota_unavailable.swap(true, Ordering::AcqRel) {
                eprintln!("Virtual drive capacity unavailable or stale: Explorer shows 0 additional free bytes until verification succeeds. This does not mean the cloud pool is full; existing reads remain available.");
            }
            Ok((used, Some(used)))
        })
    }
}
struct Handle {
    _write_lease: Option<Arc<()>>,
    drive: Arc<VirtualDrive>,
    revision: Option<Revision>,
    intent: Option<Intent>,
    file: Option<File>,
    offset: u64,
    expected: Option<u64>,
    received: u64,
    sealed: bool,
    write_stats: Arc<WriteStats>,
}
impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevisionHandle")
            .field("offset", &self.offset)
            .finish()
    }
}
impl DavFile for Handle {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let (size, tag) = if let Some(file) = &self.file {
                (
                    file.metadata().map_err(failure)?.len(),
                    self.intent.as_ref().unwrap().id.clone(),
                )
            } else {
                let r = self.revision.as_ref().ok_or(FsError::NotFound)?;
                (r.size(), r.id().into())
            };
            Ok(Box::new(Meta {
                size,
                directory: false,
                tag,
            }) as _)
        })
    }
    fn write_buf(&mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed {
                return Err(FsError::Forbidden);
            }
            let f = self.file.as_mut().ok_or(FsError::Forbidden)?;
            while buf.has_remaining() {
                let chunk = buf.chunk();
                let n = chunk.len();
                let received = self
                    .received
                    .checked_add(n as u64)
                    .ok_or(FsError::GeneralFailure)?;
                self.drive.write_spool_bytes(f, chunk).map_err(failure)?;
                self.received = received;
                self.write_stats
                    .body_bytes
                    .fetch_add(n as u64, Ordering::Relaxed);
                buf.advance(n);
            }
            Ok(())
        })
    }
    fn write_bytes(&mut self, bytes: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed {
                return Err(FsError::Forbidden);
            }
            let received = self
                .received
                .checked_add(bytes.len() as u64)
                .ok_or(FsError::GeneralFailure)?;
            self.drive
                .write_spool_bytes(self.file.as_mut().ok_or(FsError::Forbidden)?, &bytes)
                .map_err(failure)?;
            self.received = received;
            self.write_stats
                .body_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        })
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async move {
            if let Some(file) = &mut self.file {
                let mut bytes = vec![0; count];
                let n = file.read(&mut bytes).map_err(failure)?;
                bytes.truncate(n);
                return Ok(Bytes::from(bytes));
            }
            let drive = self.drive.clone();
            let revision = self.revision.clone().ok_or(FsError::Forbidden)?;
            let offset = self.offset;
            let bytes = tokio::task::spawn_blocking(move || drive.read(&revision, offset, count))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            self.offset += bytes.len() as u64;
            Ok(Bytes::from(bytes))
        })
    }
    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move {
            if let Some(file) = &mut self.file {
                return file.seek(pos).map_err(failure);
            }
            let size = self.revision.as_ref().ok_or(FsError::NotFound)?.size();
            let n = match pos {
                SeekFrom::Start(n) => n as i128,
                SeekFrom::Current(n) => self.offset as i128 + n as i128,
                SeekFrom::End(n) => size as i128 + n as i128,
            };
            if n < 0 || n > u64::MAX as i128 {
                return Err(FsError::Forbidden);
            }
            self.offset = n as u64;
            Ok(self.offset)
        })
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed || self.file.is_none() {
                return Ok(());
            }
            let file = self.file.as_ref().unwrap();
            file.sync_all().map_err(failure)?;
            // OpenOptions::size is the request body length, not the resulting
            // file length for PATCH and Content-Range writes. dav-server calls
            // flush before its own premature-EOF check, so verify before seal.
            if self.expected.is_some_and(|n| self.received != n) {
                self.write_stats.incomplete.fetch_add(1, Ordering::Relaxed);
                return Err(FsError::GeneralFailure);
            }
            let drive = self.drive.clone();
            let intent = self.intent.clone().unwrap();
            tokio::task::spawn_blocking(move || drive.seal(intent))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            self.sealed = true;
            let seals = self.write_stats.seals.fetch_add(1, Ordering::Relaxed) + 1;
            // Keep a bounded trace if an actual NFS run cannot reach clean stop.
            if seals % 64 == 0 {
                eprintln!(
                    "RPool DAV write progress: seals={} write_opens={} body_bytes={} baseline_copy_bytes={}",
                    seals,
                    self.write_stats.write_opens.load(Ordering::Relaxed),
                    self.write_stats.body_bytes.load(Ordering::Relaxed),
                    self.write_stats.baseline_copy_bytes.load(Ordering::Relaxed),
                );
            }
            Ok(())
        })
    }
}

pub(crate) struct Server {
    pub address: std::net::SocketAddr,
    pub token: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    write_stats: Arc<WriteStats>,
}
impl Server {
    pub(crate) fn start(drive: Arc<VirtualDrive>, read_only: bool) -> Result<Self> {
        super::adapter::preflight_virtual(&drive.root)?;
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
                                    } else if read_only
                                        && !matches!(
                                            req.method().as_str(),
                                            "GET" | "HEAD" | "PROPFIND" | "OPTIONS"
                                        )
                                    {
                                        hyper::Response::builder()
                                            .status(403)
                                            .body(dav_server::body::Body::from(
                                                "Read-only diagnostic mount",
                                            ))
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

fn endpoint(root: &Path) -> Result<(std::net::TcpListener, String)> {
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
        token: super::namespace::random_id()?,
    };
    // NamedTempFile is private (0600 on Unix), and is persisted atomically.
    super::namespace::durable_json(&path, &saved)?;
    Ok((listener, saved.token))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
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
                if std::net::TcpStream::connect_timeout(
                    &address,
                    std::time::Duration::from_millis(100),
                )
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

        let (_, delay) = super::super::adapter::vfs_cache_policy(true);
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
        let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
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
        let (_, write_back) = super::super::adapter::vfs_cache_policy(true);
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
            while std::net::TcpStream::connect_timeout(
                &address,
                std::time::Duration::from_millis(100),
            )
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
        let drained = super::super::adapter::drain_writeback(
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
        let server = Server::start(drive.clone(), false).unwrap();
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
        let server = Server::start(drive.clone(), false).unwrap();
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
        let server = Server::start(drive.clone(), false).unwrap();
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
    fn diagnostic_http_rejects_writes_before_spool_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
        let server = Server::start(drive.clone(), true).unwrap();
        assert!(request(&server, "PROPFIND", "/", "Depth: 0\r\n", b"", true)
            .starts_with("HTTP/1.1 207"));
        for method in [
            "PUT",
            "MKCOL",
            "DELETE",
            "MOVE",
            "COPY",
            "LOCK",
            "PROPPATCH",
        ] {
            assert!(
                request(&server, method, "/file", "", b"data", true).starts_with("HTTP/1.1 403"),
                "{method} was not rejected"
            );
        }
        assert!(drive.state.lock().unwrap().pending.is_empty());
        assert!(drive.state.lock().unwrap().directories.is_empty());
    }
    #[test]
    fn pool_sync_http_range_requests_never_mix_replaced_revisions() {
        let temp = tempfile::tempdir().unwrap();
        let mut fixture = super::super::virtual_drive::fixture(temp.path());
        fixture.pool_sync_roots = vec!["crypt:pool".into()];
        fixture.state.lock().unwrap().version = 6;
        let drive = Arc::new(fixture);
        let server = Server::start(drive.clone(), false).unwrap();
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
    #[test]
    #[ignore = "requires rclone"]
    fn rclone_about_reports_mount_capacity_for_default_64_mib_pool() {
        use super::super::capacity::CapacityStatus;
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
        let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
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
}
