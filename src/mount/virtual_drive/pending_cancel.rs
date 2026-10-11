//! Offline, selected cancellation of new pending writes. The namespace is
//! durably changed before any local bytes are removed; an interrupted cleanup
//! is resumed from a receipt before this workspace is mounted again.
use super::*;
use crate::monitor::model::QueueOperation;
use serde::{Deserialize, Serialize};
use std::io::Write;

const RECEIPTS: &str = "pending-cancellations";

#[derive(Debug, Clone)]
pub(crate) struct PendingRow {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub operation: QueueOperation,
    pub blocked: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingList {
    pub generation: u64,
    pub rows: Vec<PendingRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct CancelReport {
    pub count: usize,
    /// Logical byte sizes, not necessarily physical disk blocks freed.
    pub bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    intents: Vec<Intent>,
}

fn real_dir(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            bail!("workspace contains a reparse point");
        }
    }
    if !meta.is_dir() || meta.file_type().is_symlink() {
        bail!("workspace directory is not a real directory");
    }
    Ok(())
}

fn real_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            bail!("workspace contains a reparse point");
        }
    }
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("workspace file is not a regular file");
    }
    Ok(())
}

/// Existing stopped workspace only. The lock is held for the whole operation.
fn locked(root: &Path) -> Result<(File, Namespace)> {
    real_dir(root)?;
    for name in ["virtual.json", "namespace.json", "virtual.lock"] {
        real_file(&root.join(name))
            .with_context(|| format!("not an existing virtual drive workspace: {name}"))?;
    }
    real_dir(&root.join("spool"))?;
    real_dir(&root.join(".rpool"))?;
    crate::mount::adapter::preflight_virtual(root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("virtual.lock"))?;
    lock.try_lock()
        .context("unmount the drive before managing pending uploads")?;
    let mut state = Namespace::load(root, "pending-manager")?;
    resume(root, &mut state)?;
    Ok((lock, state))
}

fn overlaps(a: &str, b: &str) -> bool {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a + "/"))
}

/// A conservative first version: only independent, newly created writes.
fn blocker(state: &Namespace, index: usize) -> Option<&'static str> {
    let intent = &state.pending[index];
    if intent.spool.is_none() {
        return Some("deletion");
    }
    if state.pending.iter().any(|other| other.spool.is_none()) {
        // A MOVE destination can look like a fresh write. Its paired source
        // deletion has no explicit destination dependency in this format.
        return Some("pending deletion or move");
    }
    if !intent.parents.is_empty()
        || intent.depends_on.is_some()
        || intent.event_path != intent.path
        || state
            .events
            .values()
            .any(|event| overlaps(&event.path, &intent.path))
    {
        return Some("not an independent new file");
    }
    if state
        .pending
        .iter()
        .enumerate()
        .any(|(other_index, other)| {
            other_index != index
                && (other.depends_on.as_deref() == Some(&intent.id)
                    || [intent.path.as_str(), intent.event_path.as_str()]
                        .iter()
                        .any(|selected| {
                            [other.path.as_str(), other.event_path.as_str()]
                                .iter()
                                .any(|path| overlaps(selected, path))
                        }))
        })
    {
        return Some("overlapping pending change");
    }
    None
}

fn rows(state: &Namespace) -> Vec<PendingRow> {
    state
        .pending
        .iter()
        .enumerate()
        .map(|(index, intent)| PendingRow {
            id: intent.id.clone(),
            path: intent.path.clone(),
            size: intent.size,
            operation: if intent.spool.is_some() {
                QueueOperation::Write
            } else {
                QueueOperation::Delete
            },
            blocked: blocker(state, index),
        })
        .collect()
}

pub(crate) fn list(root: &Path) -> Result<PendingList> {
    let (_lock, state) = locked(root)?;
    Ok(PendingList {
        generation: state.generation,
        rows: rows(&state),
    })
}

fn verify_spool(root: &Path, intent: &Intent) -> Result<()> {
    let dir = root.join("spool").join(&intent.id);
    crate::mount::spool::real_tree(&dir)?;
    let recorded: Intent = crate::utils::read_json(&dir.join("intent.json"))?;
    if recorded != *intent || intent.spool.as_deref() != Some(&intent.id) {
        bail!("pending spool receipt does not match selected change");
    }
    let content = dir.join("content");
    let meta = fs::symlink_metadata(&content)?;
    if !meta.is_file() || meta.len() != intent.size {
        bail!("pending spool content does not match selected change");
    }
    if crate::utils::hash_file_range(&content, 0, intent.size)? != intent.hash {
        bail!("pending spool hash does not match selected change");
    }
    Ok(())
}

fn receipt_dir(root: &Path) -> PathBuf {
    root.join(".rpool").join(RECEIPTS)
}

fn canonical_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_receipt(receipt: &Receipt) -> Result<()> {
    if receipt.intents.is_empty() {
        bail!("empty pending cancellation receipt");
    }
    let mut ids = BTreeSet::new();
    for intent in &receipt.intents {
        if !canonical_id(&intent.id)
            || intent.spool.as_deref() != Some(intent.id.as_str())
            || !ids.insert(intent.id.as_str())
        {
            bail!("invalid pending cancellation receipt identity");
        }
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Completes a user-confirmed cancellation after a crash or a failed cleanup.
/// Called with the workspace lock held, before a mount can start uploading.
pub(super) fn resume(root: &Path, state: &mut Namespace) -> Result<()> {
    resume_with_verified(root, state, None)
}

/// `verified` is the receipt just written by this locked operation, whose
/// spool images were already hashed before writing it.
fn resume_with_verified(root: &Path, state: &mut Namespace, verified: Option<&Path>) -> Result<()> {
    let dir = receipt_dir(root);
    if !dir.exists() {
        return Ok(());
    }
    real_dir(&dir)?;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() != 69 || !name.ends_with(".json") || !canonical_id(&name[..64]) {
            bail!("unrecognized pending cancellation receipt");
        }
        real_file(&entry.path())?;
        let receipt: Receipt = crate::utils::read_json(&entry.path())?;
        validate_receipt(&receipt)?;
        let mut present = 0;
        for intent in &receipt.intents {
            if state.committed_intents.contains_key(&intent.id) {
                bail!("cancelled change was committed; preserving local data");
            }
            if let Some(found) = state.pending.iter().find(|p| p.id == intent.id) {
                if found != intent {
                    bail!("cancelled change identity changed; preserving local data");
                }
                present += 1;
            }
        }
        if present != 0 && present != receipt.intents.len() {
            bail!("partially changed cancellation batch; preserving local data");
        }
        if present > 0 {
            for intent in &receipt.intents {
                let index = state
                    .pending
                    .iter()
                    .position(|p| p.id == intent.id)
                    .context("selected change disappeared")?;
                if blocker(state, index).is_some() {
                    bail!("cancelled change is no longer isolated; preserving local data");
                }
                if verified != Some(entry.path().as_path()) {
                    verify_spool(root, intent)?;
                }
            }
            let ids: BTreeSet<&str> = receipt.intents.iter().map(|i| i.id.as_str()).collect();
            let mut next = state.clone();
            next.pending
                .retain(|intent| !ids.contains(intent.id.as_str()));
            next.save(root)?;
            *state = next;
        }
        // A no-op save checkpoints the cancelled state and advances the
        // previous recovery generation before any referenced bytes disappear.
        state.save(root)?;
        for intent in &receipt.intents {
            let spool = root.join("spool").join(&intent.id);
            if spool.exists() {
                crate::mount::spool::real_tree(&spool)?;
                fs::remove_dir_all(&spool)?;
            }
        }
        sync_dir(&root.join("spool"))?;
        fs::remove_file(entry.path())?;
        sync_dir(&dir)?;
    }
    Ok(())
}

pub(crate) fn cancel(root: &Path, generation: u64, selected: &[String]) -> Result<CancelReport> {
    if selected.is_empty() {
        bail!("select at least one pending upload");
    }
    let (_lock, mut state) = locked(root)?;
    if state.generation != generation {
        bail!("pending queue changed; refresh it before cancelling");
    }
    let ids: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    if ids.len() != selected.len() {
        bail!("duplicate pending upload selection");
    }
    let mut intents = Vec::with_capacity(selected.len());
    for id in selected {
        let index = state
            .pending
            .iter()
            .position(|intent| &intent.id == id)
            .context("selected upload is no longer pending; refresh the queue")?;
        if let Some(reason) = blocker(&state, index) {
            bail!("selected upload cannot be cancelled: {reason}");
        }
        let intent = state.pending[index].clone();
        verify_spool(root, &intent)?;
        intents.push(intent);
    }
    let dir = receipt_dir(root);
    fs::create_dir_all(&dir)?;
    real_dir(&dir)?;
    // The receipt directory itself must survive a power loss before a
    // namespace checkpoint can remove any of the selected intents.
    sync_dir(&root.join(".rpool"))?;
    let receipt = dir.join(format!("{}.json", random_id()?));
    // Stage outside the directory scanned by `resume`: a crash before rename
    // may leave a temporary file, but never a bogus receipt that blocks mount.
    let mut temp = tempfile::NamedTempFile::new_in(root.join(".rpool"))?;
    temp.write_all(&serde_json::to_vec(&Receipt {
        intents: intents.clone(),
    })?)?;
    temp.as_file().sync_all()?;
    crate::utils::persist_replacing(temp, &receipt)?;
    sync_dir(&dir)?;
    resume_with_verified(root, &mut state, Some(&receipt))?;
    Ok(CancelReport {
        count: intents.len(),
        bytes: intents
            .iter()
            .fold(0u64, |sum, intent| sum.saturating_add(intent.size)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        for name in ["spool", ".rpool"] {
            fs::create_dir(temp.path().join(name)).unwrap();
        }
        fs::write(temp.path().join("virtual.json"), b"{}").unwrap();
        fs::write(temp.path().join("virtual.lock"), b"").unwrap();
        let mut state = Namespace::create("test").unwrap();
        state.save(temp.path()).unwrap();
        temp
    }

    fn add(root: &Path, id_byte: u8, path: &str, bytes: &[u8]) -> Intent {
        let id = format!("{id_byte:02x}").repeat(32);
        let intent = Intent {
            id: id.clone(),
            path: path.into(),
            event_path: path.into(),
            parents: vec![],
            spool: Some(id.clone()),
            size: bytes.len() as u64,
            hash: blake3::hash(bytes).to_hex().to_string(),
            depends_on: None,
        };
        let spool = root.join("spool").join(&id);
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("content"), bytes).unwrap();
        durable_json(&spool.join("intent.json"), &intent).unwrap();
        let mut state = Namespace::load(root, "test").unwrap();
        state.pending.push(intent.clone());
        state.save(root).unwrap();
        intent
    }

    #[test]
    fn selected_new_upload_is_removed_but_other_upload_remains() {
        let temp = setup();
        let first = add(temp.path(), 1, "a.bin", b"first");
        let other = add(temp.path(), 2, "b.bin", b"second");
        let queue = list(temp.path()).unwrap();
        assert_eq!(queue.rows.len(), 2);
        assert!(queue.rows.iter().all(|row| row.blocked.is_none()));
        let result = cancel(temp.path(), queue.generation, &[first.id.clone()]).unwrap();
        assert_eq!((result.count, result.bytes), (1, 5));
        assert!(!temp.path().join("spool").join(&first.id).exists());
        assert!(temp.path().join("spool").join(&other.id).exists());
        let state = Namespace::load(temp.path(), "test").unwrap();
        assert_eq!(
            state.pending.iter().map(|i| &i.id).collect::<Vec<_>>(),
            [&other.id]
        );
        let previous = tempfile::tempdir().unwrap();
        fs::copy(
            temp.path().join("namespace.previous.json"),
            previous.path().join("namespace.json"),
        )
        .unwrap();
        if temp.path().join("namespace.previous.journal").exists() {
            fs::copy(
                temp.path().join("namespace.previous.journal"),
                previous.path().join("namespace.journal"),
            )
            .unwrap();
        }
        let old = Namespace::load(previous.path(), "test").unwrap();
        assert!(old.pending.iter().all(|i| i.id != first.id));
    }

    #[test]
    fn mounted_or_stale_workspace_refuses_without_deleting() {
        let temp = setup();
        let first = add(temp.path(), 3, "a.bin", b"keep");
        let queue = list(temp.path()).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(temp.path().join("virtual.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        assert!(cancel(temp.path(), queue.generation, &[first.id.clone()]).is_err());
        drop(lock);
        let _other = add(temp.path(), 4, "b.bin", b"more");
        assert!(cancel(temp.path(), queue.generation, &[first.id.clone()]).is_err());
        assert!(temp.path().join("spool").join(&first.id).exists());
    }

    #[test]
    fn overlapping_revisions_and_deletions_are_not_cancellable() {
        let temp = setup();
        let first = add(temp.path(), 5, "a.bin", b"first");
        let _second = add(temp.path(), 6, "a.bin", b"second");
        let queue = list(temp.path()).unwrap();
        assert!(queue.rows.iter().all(|row| row.blocked.is_some()));
        assert!(cancel(temp.path(), queue.generation, &[first.id.clone()]).is_err());

        let temp = setup();
        let first = add(temp.path(), 7, "dest.bin", b"moved");
        let mut state = Namespace::load(temp.path(), "test").unwrap();
        state.pending.push(Intent {
            id: "08".repeat(32),
            path: "source.bin".into(),
            event_path: "source.bin".into(),
            parents: vec![],
            spool: None,
            size: 0,
            hash: String::new(),
            depends_on: None,
        });
        state.save(temp.path()).unwrap();
        let queue = list(temp.path()).unwrap();
        assert!(queue.rows.iter().all(|row| row.blocked.is_some()));
        assert!(cancel(temp.path(), queue.generation, &[first.id]).is_err());
    }

    #[test]
    fn interrupted_cancellation_is_completed_before_listing() {
        let temp = setup();
        let first = add(temp.path(), 9, "a.bin", b"discard");
        let dir = receipt_dir(temp.path());
        fs::create_dir(&dir).unwrap();
        durable_json(
            &dir.join(format!("{}.json", "aa".repeat(32))),
            &Receipt {
                intents: vec![first.clone()],
            },
        )
        .unwrap();
        let mut state = Namespace::load(temp.path(), "test").unwrap();
        state.pending.clear();
        state.save(temp.path()).unwrap(); // Crash after the first save.
        assert_eq!(list(temp.path()).unwrap().rows.len(), 0);
        assert!(!temp.path().join("spool").join(&first.id).exists());
        assert!(fs::read_dir(&dir).unwrap().next().is_none());
    }

    #[test]
    fn receipt_before_first_checkpoint_is_resumed() {
        let temp = setup();
        let first = add(temp.path(), 11, "a.bin", b"discard");
        let dir = receipt_dir(temp.path());
        fs::create_dir(&dir).unwrap();
        durable_json(
            &dir.join(format!("{}.json", "ab".repeat(32))),
            &Receipt {
                intents: vec![first.clone()],
            },
        )
        .unwrap();
        assert_eq!(list(temp.path()).unwrap().rows.len(), 0);
        assert!(!temp.path().join("spool").join(&first.id).exists());
    }

    #[test]
    fn malformed_receipt_cannot_escape_spool() {
        let temp = setup();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"keep").unwrap();
        let mut intent = add(temp.path(), 12, "a.bin", b"keep");
        intent.id = "../outside".into();
        intent.spool = Some(intent.id.clone());
        let dir = receipt_dir(temp.path());
        fs::create_dir(&dir).unwrap();
        durable_json(
            &dir.join(format!("{}.json", "ac".repeat(32))),
            &Receipt {
                intents: vec![intent],
            },
        )
        .unwrap();
        assert!(list(temp.path()).is_err());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn abandoned_receipt_staging_file_does_not_block_workspace() {
        let temp = setup();
        fs::write(
            temp.path().join(".rpool").join(".tmp-previous-crash"),
            b"partial",
        )
        .unwrap();
        assert!(list(temp.path()).unwrap().rows.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn linked_spool_is_not_deleted() {
        let temp = setup();
        let first = add(temp.path(), 10, "a.bin", b"keep");
        let content = temp.path().join("spool").join(&first.id).join("content");
        fs::remove_file(&content).unwrap();
        std::os::unix::fs::symlink(temp.path().join("virtual.json"), &content).unwrap();
        let queue = list(temp.path()).unwrap();
        assert!(cancel(temp.path(), queue.generation, &[first.id.clone()]).is_err());
        assert!(Namespace::load(temp.path(), "test")
            .unwrap()
            .pending
            .iter()
            .any(|i| i.id == first.id));
    }
}
