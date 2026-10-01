//! `DavFileSystem` view of the virtual drive namespace.

use super::*;

#[derive(Clone)]
pub(super) struct VirtualFs {
    pub(super) drive: Arc<VirtualDrive>,
    pub(super) quota_unavailable: Arc<AtomicBool>,
    pub(super) write_stats: Arc<WriteStats>,
}

impl VirtualFs {
    pub(super) fn stat(&self, path: &str) -> FsResult<Meta> {
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
            self.drive.create_directory(&p).map_err(failure)
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
            match self.drive.remove_directory(&p).map_err(failure)? {
                true => Ok(()),
                false => Err(FsError::Exists),
            }
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
