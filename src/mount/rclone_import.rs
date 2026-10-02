//! Imports files stored with plain rclone (any `remote:path`) into the
//! drive as ordinary new writes, re-sharded by the normal sync. The source is
//! only listed and read. Runs offline under the workspace lock; the local
//! spool is drained by syncing between batches, and an append-only journal
//! makes an interrupted import resume without duplicates.
//!
//! Entry points: `run` (`rpool mount --import-from`) and `import` (the
//! testable core over a [`Source`]).
use super::namespace::{valid_path, Intent};
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;
use crate::storage::traits::OperationContext;

/// One entry of `rclone lsjson --recursive` output for the import source.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Listed {
    /// Path relative to the source base (`/`-separated).
    #[serde(rename = "Path")]
    pub path: String,
    /// Size in bytes; negative when rclone does not know it.
    #[serde(rename = "Size", default)]
    pub size: i64,
    /// Directory entry (recreated as an explicit drive directory).
    #[serde(rename = "IsDir", default)]
    pub is_dir: bool,
    /// rclone modification time (recorded in the journal, not applied).
    #[serde(rename = "ModTime", default)]
    pub mod_time: String,
}

/// What the importer reads. `RcloneSource` in production; tests use memory.
pub(crate) trait Source {
    /// Every entry under the source, recursively.
    fn list(&self) -> Result<Vec<Listed>>;
    /// Streams the file at `rel` into `sink` and returns the bytes read.
    fn read(&self, rel: &str, sink: &mut dyn Write) -> Result<u64>;
}

/// [`Source`] over a real rclone `remote:path`.
pub(crate) struct RcloneSource {
    /// rclone invocation context (inherits the configured environment).
    context: crate::storage::rclone::RcloneContext,
    /// Source base, e.g. `remote:` or `remote:folder`.
    base: String,
}
impl RcloneSource {
    /// Source rooted at `base` using the `rclone` binary.
    pub(crate) fn new(rclone: &str, base: &str) -> Self {
        Self {
            context: crate::storage::rclone::RcloneContext::inherited(rclone),
            base: base.into(),
        }
    }
    /// Full rclone address of `rel` under the base.
    fn address(&self, rel: &str) -> String {
        if self.base.ends_with(':') {
            format!("{}{rel}", self.base)
        } else {
            format!("{}/{rel}", self.base.trim_end_matches('/'))
        }
    }
}
impl Source for RcloneSource {
    fn list(&self) -> Result<Vec<Listed>> {
        let bytes = self
            .context
            .list_recursive(&OperationContext::none(), &self.base)
            .map_err(|e| anyhow!("cannot list {}: {e}", self.base))?;
        serde_json::from_slice(&bytes).context("invalid rclone listing")
    }
    fn read(&self, rel: &str, sink: &mut dyn Write) -> Result<u64> {
        let receipt = self
            .context
            .read_raw(&OperationContext::none(), &self.address(rel), None, sink)
            .map_err(|e| anyhow!("cannot read {rel}: {e}"))?;
        Ok(receipt.bytes_read)
    }
}

/// What to do when the destination path already exists in the drive
/// (`--import-conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OnConflict {
    /// Leave the drive's file and report the source file as skipped.
    #[default]
    Skip,
    /// Import next to it as `name (imported N).ext`.
    Rename,
}

/// Settings of one import run, built from `MountArgs` by `run`.
pub(crate) struct Options {
    /// rclone source (`remote:path`).
    pub source: String,
    /// Drive folder to import into; empty for the root.
    pub destination: String,
    /// Upload (sync) after this many imported bytes (from `--import-batch-gib`).
    pub batch_bytes: u64,
    /// Conflict policy for existing drive paths.
    pub on_conflict: OnConflict,
}

/// One journal line (JSONL); the last record per source path wins on resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum Record {
    /// Written before copying, so a crash can discard or adopt the intent.
    Begun {
        /// Source path relative to the base.
        rel: String,
        /// Destination intent created for the copy.
        intent: Intent,
    },
    /// Copied and sealed into the spool at `target`.
    Sealed {
        /// Source path relative to the base.
        rel: String,
        /// Drive path written (may be an `(imported N)` name).
        target: String,
        /// Bytes copied.
        size: u64,
        /// Source modification time from the listing.
        mod_time: String,
    },
    /// Not imported, for a stable reason (invalid name, too large, exists).
    Skipped {
        /// Source path relative to the base.
        rel: String,
        /// Human-readable reason, shown in the status and summary.
        reason: String,
    },
    /// Copy failed; retried on the next run.
    Failed {
        /// Source path relative to the base.
        rel: String,
        /// Error text.
        reason: String,
    },
}
impl Record {
    /// Source path the record belongs to.
    fn rel(&self) -> &str {
        match self {
            Self::Begun { rel, .. }
            | Self::Sealed { rel, .. }
            | Self::Skipped { rel, .. }
            | Self::Failed { rel, .. } => rel,
        }
    }
}

/// Progress for the GUI, next to the mount status file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Status {
    /// rclone source being imported.
    pub source: String,
    /// Drive folder being imported into (empty: root).
    pub destination: String,
    /// "listing", "copying", "syncing" or "done".
    pub phase: String,
    /// Files in the source listing.
    pub files_total: usize,
    /// Files processed so far (imported, skipped or failed).
    pub files_done: usize,
    /// Listed bytes of all files.
    pub bytes_total: u64,
    /// Listed bytes of processed files.
    pub bytes_done: u64,
    /// Files imported (sealed) so far, including earlier runs.
    pub imported: usize,
    /// `(source path, reason)` of skipped files.
    pub skipped: Vec<(String, String)>,
    /// `(source path, reason)` of failed files.
    pub failed: Vec<(String, String)>,
}

/// Append-only import journal (see [`journal_path`]).
struct Journal {
    /// Journal opened for appending.
    file: File,
    /// Latest record per source path, replayed at open.
    last: BTreeMap<String, Record>,
}
impl Journal {
    /// Opens (creating if needed) the journal at `path` and replays its valid
    /// lines; a torn final line is ignored.
    fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().context("journal parent")?)?;
        let mut last = BTreeMap::new();
        if path.exists() {
            let text = fs::read_to_string(path)?;
            for line in text.lines() {
                // A torn final line is the only line a crash can leave behind.
                if let Ok(record) = serde_json::from_str::<Record>(line) {
                    last.insert(record.rel().to_string(), record);
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file, last })
    }
    /// Appends `record` as one fsynced line and updates `last`.
    fn append(&mut self, record: Record) -> Result<()> {
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()?;
        self.last.insert(record.rel().to_string(), record);
        Ok(())
    }
}

/// `dir/rel`, or `rel` when `dir` is the root (empty).
fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.into()
    } else {
        format!("{dir}/{rel}")
    }
}

/// `path` renamed to `name (imported n).ext` (keeping the extension).
fn imported_name(path: &str, n: usize) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, path),
    };
    let named = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (imported {n}).{ext}"),
        _ => format!("{name} (imported {n})"),
    };
    dir.map_or(named.clone(), |d| format!("{d}/{named}"))
}

/// `Write` adapter copying the source stream into a spool image through
/// `VirtualDrive::write_spool_bytes` (spool accounting and limits).
struct SpoolSink<'a> {
    /// Drive owning the spool.
    drive: &'a VirtualDrive,
    /// Spool image file of the intent.
    file: File,
}
impl Write for SpoolSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.drive
            .write_spool_bytes(&mut self.file, bytes)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The journal of one source/destination pair in this workspace.
pub(crate) fn journal_path(root: &Path, options: &Options) -> PathBuf {
    let key = blake3::hash(format!("{}\n{}", options.source, options.destination).as_bytes());
    root.join(".rpool")
        .join("imports")
        .join(format!("{}.jsonl", &key.to_hex()[..16]))
}

/// Imports everything under `options.source`. `stop` is polled between
/// files; `status` receives progress. Returns the final status.
pub(crate) fn import(
    drive: &VirtualDrive,
    source: &dyn Source,
    options: &Options,
    stop: &dyn Fn() -> bool,
    sync: &dyn Fn() -> Result<()>,
    status: &mut dyn FnMut(&Status),
) -> Result<Status> {
    if !options.destination.is_empty() {
        valid_path(&options.destination).context("invalid drive destination folder")?;
    }
    let mut journal = Journal::open(&journal_path(&drive.root, options))?;
    let mut progress = Status {
        source: options.source.clone(),
        destination: options.destination.clone(),
        phase: "listing".into(),
        ..Default::default()
    };
    status(&progress);
    let listed = source.list()?;
    let (dirs, files): (Vec<_>, Vec<_>) = listed.into_iter().partition(|l| l.is_dir);
    progress.files_total = files.len();
    progress.bytes_total = files.iter().map(|f| f.size.max(0) as u64).sum();
    progress.phase = "copying".into();
    if !options.destination.is_empty() {
        drive.create_directory(&options.destination)?;
    }
    for dir in &dirs {
        let path = join(&options.destination, &dir.path);
        if valid_path(&path).is_ok() && !drive.state.lock().unwrap().directories.contains(&path) {
            drive.create_directory(&path)?;
        }
    }
    let mut unsynced = 0u64;
    for file in &files {
        if stop() {
            bail!("import stopped; run it again to continue");
        }
        let size = file.size.max(0) as u64;
        let outcome = import_one(
            drive,
            source,
            options,
            &mut journal,
            file,
            sync,
            &mut unsynced,
        )?;
        progress.files_done += 1;
        progress.bytes_done += size;
        match outcome {
            Some(Record::Sealed { .. }) => progress.imported += 1,
            Some(Record::Skipped { rel, reason }) => progress.skipped.push((rel, reason)),
            Some(Record::Failed { rel, reason }) => progress.failed.push((rel, reason)),
            _ => {}
        }
        if unsynced >= options.batch_bytes {
            progress.phase = "syncing".into();
            status(&progress);
            sync().context("upload between import batches failed; imported files are retained, run again to continue")?;
            unsynced = 0;
            progress.phase = "copying".into();
        }
        status(&progress);
    }
    progress.phase = "syncing".into();
    status(&progress);
    sync().context("final upload failed; imported files are retained, run again to finish")?;
    progress.phase = "done".into();
    status(&progress);
    Ok(progress)
}

/// Imports one listed file: resumes or adopts a journaled one, skips per
/// name/size/conflict rules, else begins an intent, copies, seals and
/// journals it. Syncs first when the spool would overflow.
fn import_one(
    drive: &VirtualDrive,
    source: &dyn Source,
    options: &Options,
    journal: &mut Journal,
    file: &Listed,
    sync: &dyn Fn() -> Result<()>,
    unsynced: &mut u64,
) -> Result<Option<Record>> {
    let rel = file.path.clone();
    let size = file.size.max(0) as u64;
    match journal.last.get(&rel).cloned() {
        Some(done @ (Record::Sealed { .. } | Record::Skipped { .. })) => return Ok(Some(done)),
        Some(Record::Begun { intent, .. }) => {
            let (sealed, pending_or_committed) = {
                let s = drive.state.lock().unwrap();
                let pending = s.pending.iter().any(|i| i.id == intent.id);
                (
                    pending,
                    pending || s.committed_intents.contains_key(&intent.id),
                )
            };
            if pending_or_committed {
                let record = Record::Sealed {
                    rel,
                    target: intent.path.clone(),
                    size,
                    mod_time: file.mod_time.clone(),
                };
                journal.append(record.clone())?;
                if sealed {
                    *unsynced += size;
                }
                return Ok(Some(record));
            }
            drive.discard_unsealed(&intent)?;
        }
        _ => {}
    }
    let skip = |journal: &mut Journal, reason: String| -> Result<Option<Record>> {
        let record = Record::Skipped {
            rel: rel.clone(),
            reason,
        };
        journal.append(record.clone())?;
        Ok(Some(record))
    };
    let path = join(&options.destination, &rel);
    if valid_path(&path).is_err() {
        return skip(journal, "name is not valid in the drive".into());
    }
    if size > drive.spool_limit {
        return skip(
            journal,
            "larger than the local spool limit (--spool-gib)".into(),
        );
    }
    let view = drive.view()?;
    let target = if !view.contains_key(&path) {
        path
    } else if options.on_conflict == OnConflict::Rename {
        match (1..10_000)
            .map(|n| imported_name(&path, n))
            .find(|p| !view.contains_key(p))
        {
            Some(p) => p,
            None => return skip(journal, "no free imported name".into()),
        }
    } else {
        return skip(
            journal,
            "a file with this name already exists in the drive".into(),
        );
    };
    drop(view);
    if drive.spool_bytes()?.saturating_add(size) > drive.spool_limit {
        sync().context(
            "upload to free local spool failed; imported files are retained, run again to continue",
        )?;
        *unsynced = 0;
    }
    let anc = drive.unread_ancestry(&target, false)?;
    let intent = match drive.begin_based(&target, None, &anc) {
        Ok(intent) => intent,
        Err(e) => {
            let record = Record::Failed {
                rel,
                reason: format!("{e:#}"),
            };
            journal.append(record.clone())?;
            return Ok(Some(record));
        }
    };
    journal.append(Record::Begun {
        rel: rel.clone(),
        intent: intent.clone(),
    })?;
    let copied = (|| -> Result<()> {
        let out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(drive.spool_path(&intent))?;
        let mut sink = SpoolSink { drive, file: out };
        let n = source.read(&rel, &mut sink)?;
        if file.size >= 0 && n != size {
            bail!("read {n} bytes, listing said {size}");
        }
        Ok(())
    })();
    if let Err(e) = copied {
        drive.discard_unsealed(&intent)?;
        let record = Record::Failed {
            rel,
            reason: format!("{e:#}"),
        };
        journal.append(record.clone())?;
        return Ok(Some(record));
    }
    drive.seal(intent)?;
    *unsynced += size;
    let record = Record::Sealed {
        rel,
        target,
        size,
        mod_time: file.mod_time.clone(),
    };
    journal.append(record.clone())?;
    Ok(Some(record))
}

/// `rpool mount --virtual-drive --import-from …`, after the pre-mount pull.
pub(crate) fn run(
    drive: &VirtualDrive,
    args: &crate::cli::MountArgs,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    let options = Options {
        source: args.import_from.clone().context("import source required")?,
        destination: args
            .import_to
            .as_deref()
            .unwrap_or_default()
            .trim_matches('/')
            .to_string(),
        batch_bytes: args
            .import_batch_gib
            .checked_mul(1073741824)
            .context("batch size overflow")?,
        on_conflict: args.import_conflict,
    };
    println!(
        "Importing {} into drive folder /{} (source is only read; modification times are not kept)",
        options.source, options.destination
    );
    let status_path = args
        .status_file
        .as_ref()
        .map(|p| p.with_file_name("import-status.json"));
    let mut last_print = std::time::Instant::now();
    let source = RcloneSource::new(&drive.rclone, &options.source);
    let result = import(drive, &source, &options, stop, &|| drive.sync(), &mut |s| {
        if let Some(path) = &status_path {
            let _ = super::namespace::durable_json(path, s);
        }
        if last_print.elapsed().as_secs() >= 2 || s.phase != "copying" {
            println!(
                "Import {}: {}/{} files, {}/{} bytes",
                s.phase, s.files_done, s.files_total, s.bytes_done, s.bytes_total
            );
            last_print = std::time::Instant::now();
        }
    })?;
    for (rel, reason) in &result.skipped {
        println!("  skipped {rel}: {reason}");
    }
    for (rel, reason) in &result.failed {
        eprintln!("  failed {rel}: {reason}");
    }
    println!(
        "Import finished: {} imported, {} skipped, {} failed",
        result.imported,
        result.skipped.len(),
        result.failed.len()
    );
    if !result.failed.is_empty() {
        bail!(
            "{} files could not be imported; run again to retry them",
            result.failed.len()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "rclone_import_tests.rs"]
mod tests;
