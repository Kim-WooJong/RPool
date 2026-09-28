//! Loopback-only authenticated bridge. DAV handles pin immutable revisions.
use super::{
    namespace::Intent,
    virtual_drive::{Revision, VirtualDrive},
};
use crate::prelude::*;
use bytes::{Buf, Bytes};
use dav_server::{davpath::DavPath, fs::*, DavHandler};
use std::sync::atomic::{AtomicBool, Ordering};

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
            tokio::task::spawn_blocking(move || {
                let revision = drive.view().map_err(failure)?.get(&p).cloned();
                if options.create_new && revision.is_some() {
                    return Err(FsError::Exists);
                }
                if options.write {
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
                        sealed: false,
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
                        sealed: false,
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
            let c = self.drive.capacity.lock().unwrap();
            let c = c.as_ref().ok_or(FsError::NotImplemented)?;
            Ok((c.logical_used, Some(c.logical_ceiling_estimate)))
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
    sealed: bool,
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
                self.drive.write_spool_bytes(f, chunk).map_err(failure)?;
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
            self.drive
                .write_spool_bytes(self.file.as_mut().ok_or(FsError::Forbidden)?, &bytes)
                .map_err(failure)
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
            if self
                .expected
                .is_some_and(|n| file.metadata().map(|m| m.len() != n).unwrap_or(true))
            {
                return Err(FsError::GeneralFailure);
            }
            let drive = self.drive.clone();
            let intent = self.intent.clone().unwrap();
            tokio::task::spawn_blocking(move || drive.seal(intent))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            self.sealed = true;
            Ok(())
        })
    }
}

pub(crate) struct Server {
    pub address: std::net::SocketAddr,
    pub token: String,
    stop: Arc<AtomicBool>,
}
impl Server {
    pub(crate) fn start(drive: Arc<VirtualDrive>) -> Result<Self> {
        super::adapter::preflight_virtual(&drive.root)?;
        let (listener, token) = endpoint(&drive.root)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let auth = format!("Bearer {token}");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        std::thread::spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(_) => return,
                };
                let handler = DavHandler::builder()
                    .filesystem(Box::new(VirtualFs { drive }))
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
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = hyper::service::service_fn(
                            move |req: hyper::Request<hyper::body::Incoming>| {
                                let handler = handler.clone();
                                let allowed = req
                                    .headers()
                                    .get("authorization")
                                    .is_some_and(|h| h.as_bytes() == auth.as_bytes());
                                async move {
                                    let response = if allowed {
                                        handler.handle(req).await
                                    } else {
                                        hyper::Response::builder()
                                            .status(401)
                                            .body(dav_server::body::Body::from("Unauthorized"))
                                            .unwrap()
                                    };
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
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
        });
        Ok(Self {
            address,
            token,
            stop,
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
    #[test]
    fn real_http_auth_put_range_listing_and_quota() {
        let temp = tempfile::tempdir().unwrap();
        let drive = Arc::new(super::super::virtual_drive::fixture(temp.path()));
        *drive.capacity.lock().unwrap() = Some(super::super::capacity::CapacityStatus {
            logical_used: 12,
            logical_ceiling_estimate: 112,
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
        assert!(quota.contains(">12</"), "{quota}");
        assert!(quota.contains(">100</"), "{quota}");
        let bad = request(&server, "PUT", "/%2e%2e/outside", "", b"bad", true);
        assert!(!bad.starts_with("HTTP/1.1 201"));
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
}
