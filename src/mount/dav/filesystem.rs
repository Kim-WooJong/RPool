//! `DavFileSystem` view of the virtual drive namespace.

use super::*;

#[derive(Clone)]
/// `DavFileSystem` handed to dav-server by `Server::start`; one per server.
pub(super) struct VirtualFs {
    /// Drive whose visible namespace is served.
    pub(super) drive: Arc<VirtualDrive>,
    /// Last `get_quota` had no verified capacity; used to log only state changes.
    pub(super) quota_unavailable: Arc<AtomicBool>,
    /// Write counters shared with the server.
    pub(super) write_stats: Arc<WriteStats>,
}

impl VirtualFs {
    /// Metadata of a file or directory in the visible view; directory tags change with the view generation.
    pub(super) fn stat(&self, path: &str) -> FsResult<Meta> {
        let view = self.drive.visible().map_err(failure)?;
        if let Some((id, size)) = view.file(path) {
            return Ok(Meta {
                size,
                directory: false,
                tag: id.into(),
            });
        }
        if path.is_empty()
            || view.has_descendant(&format!("{path}/"))
            || view.directories().contains(path)
        {
            return Ok(Meta {
                size: 0,
                directory: true,
                tag: format!("dir-{}", view.generation()),
            });
        }
        Err(FsError::NotFound)
    }

    /// Entries of the directory `path` in the visible view, by name; a file
    /// is `Forbidden`. Explicit (empty) folders are listed too.
    pub(super) fn list(&self, path: &str) -> FsResult<BTreeMap<String, Meta>> {
        if !self.stat(path)?.directory {
            return Err(FsError::Forbidden);
        }
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let view = self.drive.visible().map_err(failure)?;
        let mut entries = BTreeMap::new();
        for (child, file) in view.children(&prefix) {
            let meta = match file {
                Some((id, size)) => Meta {
                    size,
                    directory: false,
                    tag: id.into(),
                },
                None => Meta {
                    size: 0,
                    directory: true,
                    tag: format!("dir-{child}"),
                },
            };
            entries.insert(child, meta);
        }
        for name in view.directories() {
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
        Ok(entries)
    }
}

/// Runs namespace work on tokio's blocking pool. It takes the drive's state
/// lock, which writers hold across local disk syncs; on the server's two
/// async workers such waits stalled every other request, and rclone's NFS
/// server answers the Mac's requests one at a time behind them.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> FsResult<T> + Send + 'static,
) -> FsResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(failure)?
}

impl DavFileSystem for VirtualFs {
    fn metadata<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let (fs, p) = (self.clone(), path(p)?);
            Ok(Box::new(blocking(move || fs.stat(&p)).await?) as _)
        })
    }
    fn read_dir<'a>(
        &'a self,
        p: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let (fs, p) = (self.clone(), path(p)?);
            let entries = blocking(move || fs.list(&p)).await?;
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
                let revision = drive.visible_revision(&p).map_err(failure)?;
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
            let (fs, p) = (self.clone(), path(p)?);
            blocking(move || {
                if fs.stat(&p).is_ok() {
                    return Err(FsError::Exists);
                }
                fs.drive.create_directory(&p).map_err(failure)
            })
            .await
        })
    }
    fn remove_file<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (fs, p) = (self.clone(), path(p)?);
            blocking(move || {
                if fs.stat(&p)?.directory {
                    return Err(FsError::Forbidden);
                }
                fs.drive.delete(&p).map_err(failure)
            })
            .await
        })
    }
    fn remove_dir<'a>(&'a self, p: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let p = path(p)?;
            if p.is_empty() {
                return Err(FsError::Forbidden);
            }
            let drive = self.drive.clone();
            blocking(move || match drive.remove_directory(&p).map_err(failure)? {
                true => Ok(()),
                false => Err(FsError::Exists),
            })
            .await
        })
    }
    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let from = path(from)?;
            let to = path(to)?;
            let fs = self.clone();
            blocking(move || {
                if fs.stat(&from)?.directory {
                    fs.drive.rename_directory(&from, &to)
                } else {
                    fs.drive.rename_file(&from, &to)
                }
                .map_err(failure)
            })
            .await
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
            let drive = self.drive.clone();
            let used = blocking(move || drive.visible_used().map_err(failure)).await?;
            if !self.quota_unavailable.swap(true, Ordering::AcqRel) {
                eprintln!("Virtual drive capacity unavailable or stale: Explorer shows 0 additional free bytes until verification succeeds. This does not mean the cloud pool is full; existing reads remain available.");
            }
            Ok((used, Some(used)))
        })
    }
}
