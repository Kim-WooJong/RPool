//! Durable, single-writer local replica. Remote old versions are never deleted.
//! The plaintext files and VFS cache must survive shutdown and upload failures.
use crate::prelude::*;
use crate::utils::{append_suffix, hash_file_range, read_json};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    hash: String,
    size: u64,
    manifest: String,
    deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Catalog {
    version: u32,
    pool: String,
    policy: PoolDefinition,
    entries: BTreeMap<String, Entry>,
    directories: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared: Option<shared::SharedState>,
}

#[derive(Debug, Default)]
pub(crate) struct SyncReport {
    pub(crate) uploaded: usize,
    pub(crate) deleted: usize,
    pub(crate) unchanged: usize,
    /// Changes observed after uploading the first scan (excludes rclone's open VFS handles).
    pub(crate) pending: usize,
    pub(crate) warnings: Vec<String>,
}

pub(crate) struct Workspace {
    rclone: String,
    files: PathBuf,
    metadata: PathBuf,
    catalog: Catalog,
    _lock: File,
}

#[derive(Clone)]
struct LocalFile {
    path: PathBuf,
    hash: String,
    size: u64,
}

impl Workspace {
    pub(crate) fn open(
        rclone: &str,
        pool_name: &str,
        root: &Path,
        manifests: Vec<String>,
    ) -> Result<Self> {
        crate::pool::validate_pool_name(pool_name)?;
        if root.exists() {
            require_directory(root)?;
            if !root.join(".rpool/catalog.json").exists() && fs::read_dir(root)?.next().is_some() {
                bail!(
                    "refusing unmanaged or incompletely initialized nonempty workspace: {}",
                    root.display()
                );
            }
        } else {
            fs::create_dir_all(root)?;
        }
        let root = root.canonicalize()?;
        let metadata = root.join(".rpool");
        fs::create_dir_all(&metadata)?;
        require_directory(&metadata)?;
        let lock_path = metadata.join("workspace.lock");
        if lock_path.exists() {
            require_regular(&lock_path)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .context("workspace is already owned by another process")?;
        let catalog_path = metadata.join("catalog.json");
        let files = root.join("files");
        let catalog = if catalog_path.exists() {
            require_directory(&files).context(
                "existing workspace files directory is missing; refusing to infer deletion",
            )?;
            require_regular(&catalog_path)?;
            let saved: Catalog = read_json(&catalog_path)?;
            if !matches!(saved.version, 1 | 2)
                || saved.pool != pool_name
                || (saved.version == 2) != saved.shared.is_some()
            {
                bail!("workspace version or pool binding mismatch");
            }
            crate::pool::validate_pool(&saved.policy)?;
            for name in saved.entries.keys().chain(saved.directories.iter()) {
                validate_relative(name)?;
            }
            saved
        } else {
            let policy = crate::pool::load_pool_store()?
                .pools
                .get(pool_name)
                .cloned()
                .ok_or_else(|| anyhow!("unknown pool: {pool_name}"))?;
            crate::pool::validate_pool(&policy)?;
            let saved = Catalog {
                version: 1,
                pool: pool_name.into(),
                policy,
                entries: BTreeMap::new(),
                directories: BTreeSet::new(),
                shared: None,
            };
            fs::create_dir(&files)?;
            atomic_json(&catalog_path, &saved)?;
            saved
        };
        for dir in [
            &files,
            &metadata.join("transactions"),
            &metadata.join("archives"),
        ] {
            fs::create_dir_all(dir)?;
            require_directory(dir)?;
        }
        let mut workspace = Self {
            rclone: rclone.into(),
            files,
            metadata,
            catalog,
            _lock: lock,
        };
        for source in manifests {
            workspace.import(&source)?;
        }
        Ok(workspace)
    }

    pub(crate) fn files_dir(&self) -> &Path {
        &self.files
    }
    pub(crate) fn policy(&self) -> &PoolDefinition {
        &self.catalog.policy
    }
    pub(crate) fn pool_name(&self) -> &str {
        &self.catalog.pool
    }

    fn checkpoint(&mut self, next: Catalog) -> Result<()> {
        atomic_json(&self.metadata.join("catalog.json"), &next)?;
        self.catalog = next;
        Ok(())
    }

    fn import(&mut self, source: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(&self.rclone, source)?;
        crate::manifest::validate_manifest(&manifest)?;
        validate_component(&manifest.original_name)?;
        let name = manifest.original_name.clone();
        let fingerprint = crate::manifest::manifest_fingerprint(&manifest)?;
        let archive = self
            .metadata
            .join("archives")
            .join(format!("{fingerprint}.json"));
        let archive_name = format!("{fingerprint}.json");
        if self
            .catalog
            .entries
            .get(&name)
            .is_some_and(|entry| entry.manifest == archive_name)
        {
            return Ok(());
        }
        for entry in fs::read_dir(&self.files)? {
            let existing = entry?.file_name().to_string_lossy().to_lowercase();
            if existing == name.to_lowercase() {
                bail!("import filename collision: {name}");
            }
        }
        let dir = self
            .metadata
            .join("transactions")
            .join(format!("import-{fingerprint}"));
        fs::create_dir_all(&dir)?;
        require_directory(&dir)?;
        atomic_json(&dir.join("manifest.json"), &manifest)?;
        let staged = dir.join("content");
        crate::commands::get(
            &self.rclone,
            &dir.join("manifest.json").to_string_lossy(),
            &staged,
            self.catalog.policy.workers,
            self.catalog.policy.retries,
        )?;
        File::open(&staged)?.sync_all()?;
        let hash = hash_file_range(&staged, 0, manifest.original_size)?;
        atomic_json(&archive, &manifest)?;
        // Hard-link publication is create-if-absent (rename would overwrite a racing edit).
        fs::hard_link(&staged, self.files.join(&name))?;
        sync_directory(&self.files)?;
        let mut next = self.catalog.clone();
        next.entries.insert(
            name,
            Entry {
                hash,
                size: manifest.original_size,
                manifest: archive_name,
                deleted: false,
            },
        );
        self.checkpoint(next)?;
        fs::remove_file(staged)?;
        if let Err(error) = crate::inventory::add_manifest(&self.rclone, &archive.to_string_lossy())
        {
            eprintln!("[mount] imported; inventory index update failed: {error:#}");
        }
        Ok(())
    }

    pub(crate) fn sync_once(&mut self) -> Result<SyncReport> {
        let rclone = self.rclone.clone();
        let policy = self.catalog.policy.clone();
        let pool = self.catalog.pool.clone();
        self.sync_with(
            |source, id| {
                crate::commands::put_with_storage(
                    &crate::storage::writer::StorageWriter::rclone(&rclone),
                    &rclone,
                    source,
                    policy.remotes.clone(),
                    policy.shard_mib,
                    policy.workers,
                    policy.placement,
                    policy.retries,
                    policy.data_shards,
                    if fs::metadata(source)?.len() == 0 {
                        0
                    } else {
                        policy.parity_shards
                    },
                    Some(id.into()),
                    Some(pool.clone()),
                )?;
                let manifest_path = append_suffix(source, ".rpool.json");
                crate::commands::verify(
                    &rclone,
                    &manifest_path.to_string_lossy(),
                    true,
                    policy.workers,
                )?;
                read_json(&manifest_path)
            },
            true,
        )
    }

    fn sync_with(
        &mut self,
        mut upload: impl FnMut(&Path, &str) -> Result<Manifest>,
        index: bool,
    ) -> Result<SyncReport> {
        // Complete error-free scan precedes all mutation; unreadable trees never become deletions.
        let (files, directories) = scan(&self.files)?;
        let mut report = SyncReport::default();
        for (name, file) in &files {
            if self.catalog.entries.get(name).is_some_and(|entry| {
                !entry.deleted && entry.hash == file.hash && entry.size == file.size
            }) {
                report.unchanged += 1;
                continue;
            }
            let key = blake3::hash(format!("{name}\0{}", file.hash).as_bytes())
                .to_hex()
                .to_string();
            let transaction = self.metadata.join("transactions").join(key);
            fs::create_dir_all(&transaction)?;
            require_directory(&transaction)?;
            let id_path = transaction.join("id.json");
            let id: String = if id_path.exists() {
                read_json(&id_path)?
            } else {
                let mut bytes = [0u8; 24];
                getrandom::fill(&mut bytes)
                    .map_err(|e| anyhow!("cannot generate archive identity: {e}"))?;
                let id = format!("mount-{}", blake3::hash(&bytes).to_hex());
                atomic_json(&id_path, &id)?;
                id
            };
            let content = transaction.join("content");
            fs::create_dir_all(&content)?;
            require_directory(&content)?;
            let staged = content.join(
                Path::new(name)
                    .file_name()
                    .ok_or_else(|| anyhow!("invalid filename"))?,
            );
            if !staged.exists() {
                require_regular(&file.path)?;
                let mut temporary = tempfile::NamedTempFile::new_in(&content)?;
                let mut input = File::open(&file.path)?;
                std::io::copy(&mut input, &mut temporary)?;
                temporary.as_file().sync_all()?;
                if fs::metadata(temporary.path())?.len() != file.size
                    || hash_file_range(temporary.path(), 0, file.size)? != file.hash
                {
                    bail!("file changed during snapshot: {name}");
                }
                temporary.persist(&staged).map_err(|e| e.error)?;
                sync_directory(&content)?;
            }
            require_regular(&staged)?;
            if fs::metadata(&staged)?.len() != file.size
                || hash_file_range(&staged, 0, file.size)? != file.hash
            {
                bail!("transaction snapshot integrity failure: {name}");
            }
            let manifest = upload(&staged, &id)?;
            crate::manifest::validate_manifest(&manifest)?;
            if manifest.archive_id != id || manifest.original_size != file.size {
                bail!("upload manifest does not match transaction");
            }
            let fingerprint = crate::manifest::manifest_fingerprint(&manifest)?;
            let archive_name = format!("{fingerprint}.json");
            let archive = self.metadata.join("archives").join(&archive_name);
            atomic_json(&archive, &manifest)?;
            let mut next = self.catalog.clone();
            next.entries.insert(
                name.clone(),
                Entry {
                    hash: file.hash.clone(),
                    size: file.size,
                    manifest: archive_name,
                    deleted: false,
                },
            );
            self.checkpoint(next)?;
            report.uploaded += 1;
            if index {
                if let Err(error) =
                    crate::inventory::add_manifest(&self.rclone, &archive.to_string_lossy())
                {
                    report.warnings.push(format!(
                        "archive committed; inventory index update failed: {error:#}"
                    ));
                }
            }
            // Only remove a transaction after catalog commit. Remote objects are retained.
            fs::remove_dir_all(&transaction)?;
        }
        // Rescan after uploads: concurrent creates/deletes must not be inferred from stale scan.
        let (current, current_directories) = scan(&self.files)?;
        report.pending = current
            .iter()
            .filter(|(name, file)| {
                !self.catalog.entries.get(*name).is_some_and(|entry| {
                    !entry.deleted && entry.hash == file.hash && entry.size == file.size
                })
            })
            .count();
        let mut next = self.catalog.clone();
        for (name, entry) in &mut next.entries {
            if !entry.deleted && !current.contains_key(name) {
                entry.deleted = true;
                report.deleted += 1;
            }
        }
        next.directories = current_directories;
        if report.deleted > 0
            || next.directories != self.catalog.directories
            || directories != self.catalog.directories
        {
            self.checkpoint(next)?;
        }
        Ok(report)
    }
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing metadata parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path; // Rust does not expose portable Windows directory flush handles.
    Ok(())
}

fn require_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || is_reparse(&meta) {
        bail!("workspace requires a real directory: {}", path.display());
    }
    Ok(())
}

fn require_regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || is_reparse(&meta) {
        bail!("workspace refuses links/special files: {}", path.display());
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}

fn validate_component(name: &str) -> Result<()> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(*prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        bail!("filename is not portable to Windows: {name:?}");
    }
    Ok(())
}

fn validate_relative(name: &str) -> Result<()> {
    for component in name.split('/') {
        validate_component(component)?;
    }
    Ok(())
}

fn scan(root: &Path) -> Result<(BTreeMap<String, LocalFile>, BTreeSet<String>)> {
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    fn visit(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, LocalFile>,
        directories: &mut BTreeSet<String>,
    ) -> Result<()> {
        require_directory(dir)?;
        let mut names = BTreeSet::new();
        for item in fs::read_dir(dir)? {
            let path = item?.path();
            let component = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("non-Unicode filename is unsupported"))?;
            validate_component(component)?;
            if !names.insert(component.to_lowercase()) {
                bail!("case-insensitive filename collision");
            }
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|c| c.as_os_str().to_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("/");
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                require_directory(&path)?;
                directories.insert(relative);
                visit(root, &path, files, directories)?;
            } else {
                require_regular(&path)?;
                let size = meta.len();
                let hash = hash_file_range(&path, 0, size)?;
                if fs::symlink_metadata(&path)?.len() != size {
                    bail!("file changed while scanning");
                }
                files.insert(relative, LocalFile { path, hash, size });
            }
        }
        Ok(())
    }
    visit(root, root, &mut files, &mut directories)?;
    Ok((files, directories))
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;

#[path = "workspace_shared.rs"]
mod shared;
