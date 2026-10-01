//! Enumeration: find every archive of a pool from the local inventory and from
//! the manifest replicas stored in the cloud (so archives uploaded by other PCs
//! are included), and pick one manifest per archive. Read-only.
use crate::manifest::{manifest_fingerprint, validate_manifest};
use crate::prelude::*;
use crate::utils::{relative_remote_object, remote_join};

/// One recursive listing of a remote root.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RemoteListing {
    /// Files under the root: path relative to the root -> size. A missing root
    /// directory lists as empty.
    Listed(BTreeMap<String, u64>),
    /// The remote name is not in this PC's rclone config.
    NotConfigured,
    /// The listing failed (provider/transport error): contents unknown.
    Failed(String),
}

impl RemoteListing {
    pub(crate) fn files(&self) -> Option<&BTreeMap<String, u64>> {
        match self {
            Self::Listed(files) => Some(files),
            _ => None,
        }
    }
}

/// Copy abilities of one remote, used by the relocation estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyFeatures {
    /// A copy within this rclone remote is done by the provider.
    pub server_side_copy: bool,
    /// The crypt's base reports a hash of the stored (ciphertext) objects.
    pub ciphertext_hash: bool,
}

/// Read-only access to the cloud used by the planner (rclone in production,
/// synthetic in tests).
pub(crate) trait Cloud: Sync {
    /// Configured rclone remote names (without `:`); None when unknown.
    fn configured(&self) -> Option<BTreeSet<String>>;
    fn list(&self, remote: &str) -> RemoteListing;
    /// Reads a small metadata object (`remote:path`) or local file.
    fn read(&self, address: &str) -> Result<Vec<u8>>;
    /// Full (hashing) probe of every shard of `manifest`.
    fn probe_full(&self, manifest: &Manifest, workers: usize) -> Result<Vec<(Shard, Probe)>>;
    /// Declared outage group of a remote, if any.
    fn failure_domain(&self, remote: &str) -> Option<String>;
    /// Server-side copy and ciphertext hash support of a remote; None when
    /// unknown (the estimate then assumes a streamed copy).
    fn copy_features(&self, remote: &str) -> Option<CopyFeatures>;
    /// Conservative check that the new shards fit the target's quotas.
    fn quota_ok(
        &self,
        target: &PoolDefinition,
        specs: &[Vec<crate::models::PhysicalSpec>],
    ) -> Option<bool>;
    /// Quota of every target account, None when any is unknown.
    fn quotas(
        &self,
        _target: &PoolDefinition,
    ) -> Option<Vec<crate::storage::admin::budget::TargetBudget>> {
        None
    }
}

/// Drive revisions (`virtual-*`) are handled by a later phase.
pub(crate) fn is_drive_archive(archive_id: &str) -> bool {
    archive_id.starts_with("virtual-")
}

/// Parses `rclone lsjson -R` output into file path -> size.
pub(crate) fn parse_lsjson(bytes: &[u8]) -> Result<BTreeMap<String, u64>> {
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "Path")]
        path: String,
        #[serde(rename = "Size", default)]
        size: i64,
        #[serde(rename = "IsDir", default)]
        is_dir: bool,
    }
    let items: Vec<Item> = serde_json::from_slice(bytes).context("invalid rclone lsjson output")?;
    Ok(items
        .into_iter()
        .filter(|item| !item.is_dir && item.size >= 0)
        .map(|item| (item.path, item.size as u64))
        .collect())
}

/// Archive ids with a `<id>/manifest.json` replica in a root listing. Dot
/// directories (`.rpool-sync` bookkeeping) are skipped.
pub(crate) fn manifest_ids(files: &BTreeMap<String, u64>) -> Vec<(String, u64)> {
    files
        .iter()
        .filter_map(|(path, size)| {
            let id = path.strip_suffix("/manifest.json")?;
            (!id.is_empty() && !id.contains('/') && !id.starts_with('.'))
                .then(|| (id.to_owned(), *size))
        })
        .collect()
}

fn on_remotes(target: &BTreeSet<String>, address: &str) -> bool {
    target
        .iter()
        .any(|remote| relative_remote_object(remote, address).is_ok())
}

/// An inventory entry belongs to the pool when a shard or its manifest source
/// is on a pool remote.
pub(crate) fn in_pool(entry: &InventoryEntry, target: &BTreeSet<String>) -> bool {
    entry.remotes.iter().any(|r| target.contains(r)) || on_remotes(target, &entry.manifest_source)
}

/// A manifest chosen for one archive.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    pub manifest: Manifest,
    pub source: String,
    pub fingerprint: String,
}

/// An archive of the pool.
#[derive(Debug, Clone)]
pub(crate) enum Found {
    Loaded(Loaded),
    /// No valid manifest could be read; known fields for the entry.
    Unreadable {
        archive_id: String,
        original_name: String,
        size: u64,
        source: String,
        detail: String,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Enumerated {
    pub found: Vec<Found>,
    pub notes: Vec<String>,
    pub drive_skipped: usize,
}

/// Candidate manifest locations of one archive.
#[derive(Debug, Default, Clone)]
struct Sources {
    /// Inventory manifest source (local path or remote address).
    inventory: Option<String>,
    /// Replica address -> listed size.
    replicas: BTreeMap<String, u64>,
    name: String,
    size: u64,
}

/// Collects archives of the pool. `target` are the resolved pool remotes;
/// `listings` must contain every target remote and may contain others (whose
/// replicas are only used for archives already known).
pub(crate) fn enumerate(
    cloud: &dyn Cloud,
    target: &BTreeSet<String>,
    inventory: &InventoryStore,
    listings: &BTreeMap<String, RemoteListing>,
) -> Enumerated {
    let mut out = Enumerated::default();
    let mut archives: BTreeMap<String, Sources> = BTreeMap::new();
    for entry in inventory.entries.values() {
        if !in_pool(entry, target) {
            continue;
        }
        if is_drive_archive(&entry.archive_id) {
            out.drive_skipped += 1;
            continue;
        }
        let sources = archives.entry(entry.archive_id.clone()).or_default();
        sources.inventory = Some(entry.manifest_source.clone());
        sources.name.clone_from(&entry.original_name);
        sources.size = entry.original_size;
    }
    let mut drive_ids = BTreeSet::new();
    for remote in target {
        if let Some(files) = listings.get(remote).and_then(RemoteListing::files) {
            for (id, size) in manifest_ids(files) {
                if is_drive_archive(&id) {
                    drive_ids.insert(id);
                    continue;
                }
                archives
                    .entry(id.clone())
                    .or_default()
                    .replicas
                    .insert(remote_join(remote, &format!("{id}/manifest.json")), size);
            }
        }
    }
    out.drive_skipped += drive_ids.len();
    // Replicas on other listed remotes (removed accounts) only for known archives.
    for (remote, listing) in listings {
        if target.contains(remote) {
            continue;
        }
        if let Some(files) = listing.files() {
            for (id, size) in manifest_ids(files) {
                if let Some(sources) = archives.get_mut(&id) {
                    sources
                        .replicas
                        .insert(remote_join(remote, &format!("{id}/manifest.json")), size);
                }
            }
        }
    }
    let results: Vec<(Found, Vec<String>)> = archives
        .into_par_iter()
        .map(|(id, sources)| choose(cloud, &id, &sources, target))
        .collect();
    for (found, notes) in results {
        out.found.push(found);
        out.notes.extend(notes);
    }
    out
}

fn load_one(cloud: &dyn Cloud, id: &str, address: &str) -> Result<Loaded> {
    let bytes = cloud.read(address)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).context("invalid rpool manifest")?;
    validate_manifest(&manifest)?;
    if manifest.archive_id != id {
        bail!(
            "archive id mismatch: expected {id}, found {}",
            manifest.archive_id
        );
    }
    Ok(Loaded {
        fingerprint: manifest_fingerprint(&manifest)?,
        manifest,
        source: address.to_owned(),
    })
}

/// Reads as few replicas as possible: the inventory manifest (usually local)
/// and one cloud replica per distinct listed size, target remotes first.
fn choose(
    cloud: &dyn Cloud,
    id: &str,
    sources: &Sources,
    target: &BTreeSet<String>,
) -> (Found, Vec<String>) {
    let mut notes = Vec::new();
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut errors = Vec::new();
    let mut read_sizes = BTreeSet::new();
    if let Some(source) = &sources.inventory {
        match load_one(cloud, id, source) {
            Ok(found) => {
                if let Ok(meta) = fs::metadata(source) {
                    read_sizes.insert(meta.len());
                }
                loaded.push(found);
            }
            Err(error) => errors.push(format!("{source}: {error:#}")),
        }
    }
    let mut replicas: Vec<(&String, &u64)> = sources.replicas.iter().collect();
    // Target replicas before replicas on removed accounts.
    replicas.sort_by_key(|(address, _)| !on_remotes(target, address));
    for (address, size) in replicas {
        if Some(address) == sources.inventory.as_ref() {
            continue;
        }
        let have_valid = !loaded.is_empty();
        if have_valid && read_sizes.contains(size) {
            continue;
        }
        match load_one(cloud, id, address) {
            Ok(found) => {
                read_sizes.insert(*size);
                loaded.push(found);
            }
            Err(error) => errors.push(format!("{address}: {error:#}")),
        }
    }
    let distinct: BTreeSet<&str> = loaded.iter().map(|l| l.fingerprint.as_str()).collect();
    if distinct.len() > 1 {
        notes.push(format!(
            "{id}: {} different manifests found; the newest was used",
            distinct.len()
        ));
    }
    if !errors.is_empty() && !loaded.is_empty() {
        notes.push(format!(
            "{id}: {} manifest replica(s) unreadable or invalid",
            errors.len()
        ));
    }
    let best = loaded.into_iter().max_by(|a, b| {
        (a.manifest.created_unix, &a.fingerprint).cmp(&(b.manifest.created_unix, &b.fingerprint))
    });
    let found = match best {
        Some(best) => Found::Loaded(best),
        None => Found::Unreadable {
            archive_id: id.to_owned(),
            original_name: sources.name.clone(),
            size: sources.size,
            source: sources
                .inventory
                .clone()
                .or_else(|| sources.replicas.keys().next().cloned())
                .unwrap_or_default(),
            detail: format!("no readable valid manifest: {}", errors.join("; ")),
        },
    };
    (found, notes)
}

#[cfg(test)]
#[path = "enumerate_tests.rs"]
mod tests;
