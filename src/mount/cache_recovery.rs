//! Recovers writes a previous WebDAV session left unsaved in rclone's VFS
//! cache (a crash before write-back) instead of stranding them. Only dirty,
//! complete entries are imported, as ordinary pending writes; nothing in the
//! drive is overwritten without the ancestry a WebDAV write would have had.
//! Everything else is kept in `recovered-native-cache/<id>` and reported.
#[path = "cache_recovery_scan.rs"]
mod scan;

use super::namespace::{durable_json, random_id, valid_path};
use super::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;
use scan::{CacheEntry, EntryKind};

const JOURNAL: &str = "rpool-recovery.json";
pub(crate) const REPORT: &str = "cache-recovery.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(crate) enum Outcome {
    /// Imported at its own path as the next write.
    Imported {
        intent: String,
    },
    /// Imported next to the original as a named copy.
    Copied {
        target: String,
        intent: String,
    },
    Skipped {
        reason: String,
    },
    Kept {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Source {
    /// The cache of the last WebDAV session of this workspace.
    DavCache,
    /// Set aside by an older RPool without a journal; native sessions may
    /// have edited these paths since, so entries only become named copies.
    Legacy,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    source: Source,
    done: bool,
    #[serde(default)]
    entries: BTreeMap<String, Outcome>,
}

/// Summary of one recovered cache directory, also saved for the GUI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecoveryReport {
    pub dir: PathBuf,
    pub imported: Vec<String>,
    pub copied: Vec<(String, String)>,
    pub kept: Vec<(String, String)>,
    pub skipped: usize,
    pub clean: usize,
    /// The directory held nothing to keep and was removed.
    pub removed: bool,
}

impl VirtualDrive {
    /// Freezes a leftover rclone cache and recovers every unfinished frozen
    /// cache of this workspace. Run under the workspace lock, after `pull`
    /// and before any frontend starts.
    pub(crate) fn recover_previous_cache(&self) -> Result<Vec<RecoveryReport>> {
        super::adapter::preflight_virtual(&self.root)?;
        let recovery = self.root.join("recovered-native-cache");
        self.freeze_cache(&recovery)?;
        if !recovery.exists() {
            return Ok(Vec::new());
        }
        super::retention::real_tree(&recovery)?;
        let mut dirs: Vec<PathBuf> = fs::read_dir(&recovery)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        dirs.sort();
        let mut reports = Vec::new();
        for dir in dirs.into_iter().filter(|d| d.is_dir()) {
            if let Some(report) = self.recover_dir(&dir)? {
                reports.push(report);
            }
        }
        if !reports.is_empty() {
            let status = self.root.join(".rpool");
            fs::create_dir_all(&status)?;
            durable_json(&status.join(REPORT), &reports)?;
            for report in &reports {
                print_report(report);
            }
        }
        Ok(reports)
    }

    fn freeze_cache(&self, recovery: &Path) -> Result<()> {
        let cache = self.root.join("vfs-cache");
        if !cache.exists() {
            return Ok(());
        }
        super::retention::real_tree(&cache)?;
        if fs::read_dir(&cache)?.next().is_none() {
            return Ok(());
        }
        // The journal goes in before the rename, so a frozen cache always
        // knows it came from this workspace's last WebDAV session.
        if !cache.join(JOURNAL).exists() {
            durable_json(&cache.join(JOURNAL), &journal(Source::DavCache))?;
        }
        fs::create_dir_all(recovery)?;
        fs::rename(&cache, recovery.join(random_id()?))?;
        #[cfg(unix)]
        {
            File::open(recovery)?.sync_all()?;
            File::open(&self.root)?.sync_all()?;
        }
        Ok(())
    }

    fn recover_dir(&self, dir: &Path) -> Result<Option<RecoveryReport>> {
        let path = dir.join(JOURNAL);
        let mut journal = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("unreadable recovery journal {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let legacy = journal(Source::Legacy);
                durable_json(&path, &legacy)?;
                legacy
            }
            Err(e) => return Err(e.into()),
        };
        if journal.done {
            return Ok(None);
        }
        let entries = match scan::scan(dir)? {
            Ok(entries) => entries,
            Err(reason) => {
                eprintln!(
                    "Previous WebDAV cache kept unread at {}: {reason}",
                    dir.display()
                );
                journal.done = true;
                durable_json(&path, &journal)?;
                return Ok(Some(RecoveryReport {
                    dir: dir.into(),
                    kept: vec![(String::new(), reason)],
                    ..Default::default()
                }));
            }
        };
        let tag: String = dir
            .file_name()
            .map(|n| n.to_string_lossy().chars().take(8).collect())
            .unwrap_or_default();
        let mut report = RecoveryReport {
            dir: dir.into(),
            ..Default::default()
        };
        for entry in &entries {
            let outcome = match journal.entries.get(&entry.rel) {
                Some(done) => done.clone(),
                None => match &entry.kind {
                    EntryKind::Clean => {
                        report.clean += 1;
                        continue;
                    }
                    EntryKind::Incomplete => Outcome::Kept {
                        reason: "only part of the file was cached".into(),
                    },
                    EntryKind::Unknown(reason) => Outcome::Kept {
                        reason: reason.clone(),
                    },
                    EntryKind::Dirty => {
                        let outcome = self.import_entry(entry, &tag, journal.source)?;
                        journal.entries.insert(entry.rel.clone(), outcome.clone());
                        super::crash::point("cache_recovery.before_journal")?;
                        durable_json(&path, &journal)?;
                        outcome
                    }
                },
            };
            match outcome {
                Outcome::Imported { .. } => report.imported.push(entry.rel.clone()),
                Outcome::Copied { target, .. } => report.copied.push((entry.rel.clone(), target)),
                Outcome::Skipped { .. } => report.skipped += 1,
                Outcome::Kept { reason } => report.kept.push((entry.rel.clone(), reason)),
            }
        }
        journal.done = true;
        durable_json(&path, &journal)?;
        // Clean entries are a read cache of the drive and dirty ones are now
        // sealed pending writes, so nothing else is lost by removing it.
        if report.kept.is_empty() {
            fs::remove_dir_all(dir)?;
            report.removed = true;
        }
        let quiet = report.imported.is_empty() && report.copied.is_empty() && report.skipped == 0;
        Ok((!quiet || !report.kept.is_empty()).then_some(report))
    }

    fn import_entry(&self, entry: &CacheEntry, tag: &str, source: Source) -> Result<Outcome> {
        if valid_path(&entry.rel).is_err() {
            return Ok(Outcome::Kept {
                reason: "name is not valid in the drive".into(),
            });
        }
        let hash = match &entry.data {
            Some(data) => crate::utils::hash_file_range(data, 0, entry.size)?,
            None => Hasher::new().finalize().to_hex().to_string(),
        };
        let view = self.view()?;
        let head = view.get(&entry.rel);
        if let Some(head) = head {
            if revision_hash(head)? == hash {
                return Ok(skipped("already in the drive"));
            }
        }
        // An empty base only records that the path was new when first written.
        let has_base = self
            .state
            .lock()
            .unwrap()
            .bases
            .get(&entry.rel)
            .is_some_and(|b| !b.is_empty());
        // A WebDAV write descends from the revision last read (`bases`),
        // else from what is visible. Without a base, a peer may have written
        // the path meanwhile, so only a single-writer drive, or a path whose
        // head is still this workspace's own unsynced write, writes in place.
        let own_head = matches!(head, Some(Revision::Local { .. }));
        let in_place = source == Source::DavCache
            && (head.is_none() || has_base || own_head || self.pool_sync_roots.is_empty());
        let (target, visible) = if in_place {
            (entry.rel.clone(), head.cloned())
        } else {
            let target = recovered_name(&entry.rel, tag, &hash);
            if let Some(existing) = view.get(&target) {
                return Ok(if revision_hash(existing)? == hash {
                    skipped("recovered copy already in the drive")
                } else {
                    Outcome::Kept {
                        reason: format!("{target} already exists"),
                    }
                });
            }
            (target, None)
        };
        let intent = match self.begin_observed(&target, visible.as_ref()) {
            Ok(intent) => intent,
            Err(e) => {
                return Ok(Outcome::Kept {
                    reason: format!("cannot write here: {e:#}"),
                })
            }
        };
        if let Err(e) = self.copy_cached(entry, &intent) {
            self.discard_unsealed(&intent)?;
            return Ok(Outcome::Kept {
                reason: format!("copy failed: {e:#}"),
            });
        }
        let id = intent.id.clone();
        self.seal(intent)?;
        Ok(if in_place {
            Outcome::Imported { intent: id }
        } else {
            Outcome::Copied { target, intent: id }
        })
    }

    fn copy_cached(&self, entry: &CacheEntry, intent: &super::namespace::Intent) -> Result<()> {
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.spool_path(intent))?;
        let Some(data) = &entry.data else {
            return Ok(());
        };
        let mut input = File::open(data)?.take(entry.size);
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut copied = 0u64;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            self.write_spool_bytes(&mut out, &buffer[..n])?;
            copied += n as u64;
        }
        if copied != entry.size {
            bail!("cached data is shorter than its recorded size");
        }
        Ok(())
    }
}

fn journal(source: Source) -> Journal {
    Journal {
        version: 1,
        source,
        done: false,
        entries: BTreeMap::new(),
    }
}

fn skipped(reason: &str) -> Outcome {
    Outcome::Skipped {
        reason: reason.into(),
    }
}

fn revision_hash(revision: &Revision) -> Result<String> {
    match revision {
        Revision::Cloud { content, .. } => Ok(content.hash.clone()),
        Revision::Local { path, size, .. } => crate::utils::hash_file_range(path, 0, *size),
    }
}

/// `dir/name (recovered <tag>-<hash6>).ext`: deterministic, so a rerun finds
/// the copy it already made.
pub(crate) fn recovered_name(path: &str, tag: &str, hash: &str) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, path),
    };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (name, None),
    };
    let suffix = &hash[..hash.len().min(6)];
    let mut out = format!("{stem} (recovered {tag}-{suffix})");
    if let Some(ext) = ext {
        out.push('.');
        out.push_str(ext);
    }
    match dir {
        Some(dir) => format!("{dir}/{out}"),
        None => out,
    }
}

fn print_report(report: &RecoveryReport) {
    println!(
        "Recovered unsaved WebDAV cache {}: {} at original path, {} as recovered copies, {} kept, {} already present",
        report.dir.display(),
        report.imported.len(),
        report.copied.len(),
        report.kept.len(),
        report.skipped,
    );
    for (from, to) in &report.copied {
        println!("  recovered copy: {from} -> {to}");
    }
    for (path, reason) in &report.kept {
        println!("  kept in cache folder: {path} ({reason})");
    }
}

#[cfg(test)]
#[path = "cache_recovery_tests.rs"]
mod tests;
