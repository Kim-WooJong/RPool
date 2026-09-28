//! Single externally designated coordinator; generic rclone provides no CAS.
//! Publication is only a proposal. Only an activated checkpoint acknowledges it.
use super::shared_checkpoint_model::{Checkpoint, ManagedContent, Proposal};
use crate::prelude::*;
use crate::storage::{
    error::{StorageError, StorageErrorKind},
    rclone::RcloneContext,
    traits::OperationContext,
    writer::StorageWriter,
};
const LIMIT: usize = 64 * 1024 * 1024;
const BATCH: usize = 256;

pub(crate) trait CheckpointIo {
    fn read(&self, relative: &str) -> Result<Option<Vec<u8>>>;
    fn put(&self, relative: &str, bytes: &[u8]) -> Result<()>;
    fn list(&self, relative_dir: &str) -> Result<Vec<String>>;
    fn remove(&self, relative: &str) -> Result<()>;
    fn remove_object(&self, raw: &str) -> Result<()>;
    fn verify(&self, content: &ManagedContent) -> Result<()>;
    fn identity(&self) -> String {
        "synthetic".into()
    }
    fn validate_scope(&self, value: &ManagedContent) -> Result<()> {
        value.validate()
    }
}
pub(crate) struct RcloneIo {
    rclone: String,
    root: String,
}
fn not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::NotFound)
}
fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains([':', '\\'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        bail!("invalid checkpoint relative path");
    }
    Ok(())
}
impl RcloneIo {
    pub(crate) fn new(rclone: &str, root: &str) -> Result<Self> {
        super::shared_transport::SharedTransport::new(rclone, root)?;
        Ok(Self {
            rclone: rclone.into(),
            root: root.trim_end_matches('/').into(),
        })
    }
    pub(crate) fn root_hash(&self) -> String {
        blake3::hash(self.root.as_bytes()).to_hex().to_string()
    }
    fn address(&self, path: &str) -> Result<String> {
        relative(path)?;
        Ok(crate::utils::remote_join(&self.root, path))
    }
}
impl CheckpointIo for RcloneIo {
    fn identity(&self) -> String {
        self.root.clone()
    }
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let address = self.address(path)?;
        let storage = StorageWriter::rclone(&self.rclone);
        storage.ensure_destination(&address)?;
        let metadata = match storage.reader().stat(&address) {
            Ok(m) => m,
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        if metadata.is_dir || metadata.size > LIMIT as u64 {
            bail!("checkpoint metadata exceeds bound");
        }
        let bytes = storage.reader().read_metadata(&address)?;
        if bytes.len() > LIMIT || bytes.len() as u64 != metadata.size {
            bail!("checkpoint metadata changed or exceeds bound");
        }
        Ok(Some(bytes))
    }
    fn put(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > LIMIT {
            bail!("checkpoint metadata exceeds bound");
        }
        StorageWriter::rclone(&self.rclone).write_bytes(&self.address(path)?, bytes, 1)?;
        if self.read(path)?.as_deref() != Some(bytes) {
            bail!("checkpoint write readback mismatch");
        }
        Ok(())
    }
    fn list(&self, path: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Entry {
            #[serde(rename = "Path")]
            path: String,
            #[serde(rename = "Size")]
            size: i64,
            #[serde(rename = "IsDir")]
            dir: bool,
        }
        let address = self.address(path)?;
        let context = RcloneContext::inherited(&self.rclone);
        let op = OperationContext::none();
        context.ensure_crypt(&op, &address)?;
        let bytes = match context.capture(&op, &["lsjson", "--files-only", "--", &address]) {
            Ok(b) => b,
            Err(e) if e.kind() == StorageErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        if bytes.len() > LIMIT {
            bail!("checkpoint listing exceeds bound");
        }
        let mut result = BTreeSet::new();
        for e in serde_json::from_slice::<Vec<Entry>>(&bytes)? {
            relative(&e.path)?;
            if e.dir
                || e.path.contains('/')
                || e.size < 0
                || e.size as usize > LIMIT
                || !result.insert(e.path)
            {
                bail!("invalid checkpoint listing entry");
            }
        }
        Ok(result.into_iter().collect())
    }
    fn remove(&self, path: &str) -> Result<()> {
        self.remove_object(&self.address(path)?)
    }
    fn remove_object(&self, raw: &str) -> Result<()> {
        let storage = StorageWriter::rclone(&self.rclone);
        storage.ensure_destination(raw)?;
        match storage.delete(raw) {
            Ok(()) => Ok(()),
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn validate_scope(&self, value: &ManagedContent) -> Result<()> {
        value.validate()?;
        if !value.objects.is_empty()
            && !value
                .content
                .manifest
                .archive_id
                .starts_with(&format!("epoch-{}-", self.root_hash()))
        {
            bail!("archive belongs to another shared root");
        }
        Ok(())
    }
    fn verify(&self, value: &ManagedContent) -> Result<()> {
        self.validate_scope(value)?;
        let storage = StorageWriter::rclone(&self.rclone);
        for shard in &value.content.manifest.shards {
            storage.ensure_destination(&shard.object)?;
            storage.reader().verify(shard, true)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Pointer {
    pub version: u32,
    pub owner: String,
    pub serial: u64,
    pub hash: String,
}
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn checkpoint_path(pointer: &Pointer) -> String {
    format!("checkpoints/{}.json", pointer.hash)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > LIMIT {
        bail!("checkpoint metadata exceeds bound");
    }
    Ok(serde_json::from_slice(bytes)?)
}
pub(crate) fn load(io: &dyn CheckpointIo) -> Result<Option<(Pointer, Checkpoint)>> {
    let Some(bytes) = io.read("current.json")? else {
        return Ok(None);
    };
    let pointer: Pointer = decode(&bytes)?;
    if pointer.version != 5
        || pointer.hash.len() != 64
        || !pointer
            .hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("invalid checkpoint pointer");
    }
    let bytes = io
        .read(&checkpoint_path(&pointer))?
        .context("active checkpoint missing")?;
    if hash(&bytes) != pointer.hash {
        bail!("checkpoint hash mismatch");
    }
    let checkpoint: Checkpoint = decode(&bytes)?;
    checkpoint.validate()?;
    if checkpoint.serial != pointer.serial || checkpoint.owner != pointer.owner {
        bail!("checkpoint pointer identity mismatch");
    }
    Ok(Some((pointer, checkpoint)))
}
fn immutable(io: &dyn CheckpointIo, path: &str, bytes: &[u8]) -> Result<()> {
    if let Some(old) = io.read(path)? {
        if old != bytes {
            bail!("immutable checkpoint object differs");
        }
        return Ok(());
    }
    io.put(path, bytes)
}
pub(crate) fn publish_proposal(io: &dyn CheckpointIo, proposal: &Proposal) -> Result<()> {
    proposal.validate()?;
    immutable(
        io,
        &format!("proposals/{}-{}.json", proposal.serial, proposal.id),
        &serde_json::to_vec(proposal)?,
    )
}
#[derive(Serialize, Deserialize)]
struct Transition {
    identity: String,
    expected: Option<Pointer>,
    next: Pointer,
    checkpoint: Checkpoint,
    objects: Vec<String>,
    proposals: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    hash: String,
    payload: Transition,
}
fn save(path: &Path, payload: &Transition) -> Result<()> {
    super::namespace::durable_json(
        path,
        &Journal {
            hash: hash(&serde_json::to_vec(payload)?),
            payload: Transition {
                identity: payload.identity.clone(),
                expected: payload.expected.clone(),
                next: payload.next.clone(),
                checkpoint: payload.checkpoint.clone(),
                objects: payload.objects.clone(),
                proposals: payload.proposals.clone(),
            },
        },
    )
}
/// Enforce the durable local high-water on EVERY pointer observation, not only
/// a preflight read. A stale backend response must never authorize rollback GC.
struct MonotonicIo<'a> {
    inner: &'a dyn CheckpointIo,
    floor: Option<&'a super::namespace::CheckpointCursor>,
}
impl MonotonicIo<'_> {
    fn check(&self, bytes: Option<&[u8]>) -> Result<()> {
        if let Some(floor) = self.floor {
            let p: Pointer =
                decode(bytes.context("active checkpoint missing after initialization")?)?;
            if p.owner != floor.owner
                || p.serial < floor.serial
                || (p.serial == floor.serial && p.hash != floor.hash)
            {
                bail!("checkpoint is below durable high-water mark");
            }
        }
        Ok(())
    }
}
impl CheckpointIo for MonotonicIo<'_> {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let bytes = self.inner.read(path)?;
        if path == "current.json" {
            self.check(bytes.as_deref())?;
        }
        Ok(bytes)
    }
    fn put(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if path == "current.json" {
            self.check(Some(bytes))?;
        }
        self.inner.put(path, bytes)
    }
    fn list(&self, path: &str) -> Result<Vec<String>> {
        self.inner.list(path)
    }
    fn remove(&self, path: &str) -> Result<()> {
        self.inner.remove(path)
    }
    fn remove_object(&self, path: &str) -> Result<()> {
        self.inner.remove_object(path)
    }
    fn verify(&self, value: &ManagedContent) -> Result<()> {
        self.inner.verify(value)
    }
    fn identity(&self) -> String {
        self.inner.identity()
    }
    fn validate_scope(&self, value: &ManagedContent) -> Result<()> {
        self.inner.validate_scope(value)
    }
}
pub(crate) fn coordinate(
    io: &dyn CheckpointIo,
    local_root: &Path,
    device: &str,
    keep: usize,
    seed: Option<Checkpoint>,
    minimum: Option<&super::namespace::CheckpointCursor>,
) -> Result<(Pointer, Checkpoint)> {
    let guard = MonotonicIo {
        inner: io,
        floor: minimum,
    };
    let io: &dyn CheckpointIo = &guard;
    let path = local_root.join("shared-checkpoint-transition.json");
    let mut journal = if path.exists() {
        let stored: Journal = decode(&fs::read(&path)?)?;
        if stored.hash != hash(&serde_json::to_vec(&stored.payload)?) {
            bail!("checkpoint transition checksum mismatch");
        }
        stored.payload
    } else {
        let active = load(io)?;
        let (expected, checkpoint) = match active {
            Some((p, c)) => (Some(p), c),
            None => (
                None,
                seed.context("shared root requires explicit coordinator initialization")?,
            ),
        };
        checkpoint.validate()?;
        if checkpoint.owner != device {
            bail!("only designated coordinator may advance checkpoint");
        }
        for v in checkpoint
            .files
            .values()
            .chain(checkpoint.history.values().flatten())
        {
            io.validate_scope(&v.value)?;
        }
        let mut proposals = Vec::new();
        let mut names = io.list("proposals")?;
        names.sort();
        let mut processed = vec![];
        for name in names.into_iter().take(BATCH) {
            relative(&name)?;
            if name.contains('/') {
                bail!("invalid proposal filename");
            }
            let key = format!("proposals/{name}");
            let Some(bytes) = io.read(&key)? else {
                continue;
            };
            let proposal: Proposal = decode(&bytes)?;
            proposal.validate()?;
            if name != format!("{}-{}.json", proposal.serial, proposal.id) {
                bail!("proposal filename identity mismatch");
            }
            if proposal.serial > checkpoint.serial {
                bail!("proposal references future checkpoint");
            }
            if proposal.serial == checkpoint.serial {
                if let Some(value) = &proposal.value {
                    io.verify(value)?;
                }
                proposals.push(proposal);
            }
            processed.push(key);
        }
        let needs_trim = checkpoint.history.values().any(|h| h.len() > keep);
        if expected.is_some() && processed.is_empty() && !needs_trim {
            return Ok((expected.unwrap(), checkpoint));
        }
        let (next_checkpoint, objects) =
            if proposals.is_empty() && (!processed.is_empty() || needs_trim) {
                checkpoint.compact(keep)?
            } else if proposals.is_empty() {
                (checkpoint, vec![])
            } else {
                checkpoint.apply_batch(&proposals, keep)?
            };
        next_checkpoint.validate()?;
        let next = Pointer {
            version: 5,
            owner: device.into(),
            serial: next_checkpoint.serial,
            hash: hash(&serde_json::to_vec(&next_checkpoint)?),
        };
        let transition = Transition {
            identity: io.identity(),
            expected,
            next,
            checkpoint: next_checkpoint,
            objects,
            proposals: processed,
        };
        save(&path, &transition)?;
        transition
    };
    if journal.identity != io.identity()
        || journal.next.owner != device
        || journal.checkpoint.owner != device
    {
        bail!("checkpoint transition owner/root mismatch");
    }
    journal.checkpoint.validate()?;
    let bytes = serde_json::to_vec(&journal.checkpoint)?;
    if hash(&bytes) != journal.next.hash || journal.checkpoint.serial != journal.next.serial {
        bail!("checkpoint transition identity mismatch");
    }
    let current = load(io)?.map(|(p, _)| p);
    if current != journal.expected && current.as_ref() != Some(&journal.next) {
        bail!("checkpoint changed outside designated transition");
    }
    // Verification precedes activation AND every resumed destructive sweep.
    for version in journal
        .checkpoint
        .files
        .values()
        .chain(journal.checkpoint.history.values().flatten())
    {
        io.verify(&version.value)?;
    }
    immutable(io, &checkpoint_path(&journal.next), &bytes)?;
    if current.as_ref() != Some(&journal.next) {
        io.put("current.json", &serde_json::to_vec(&journal.next)?)?;
    }
    if load(io)?.map(|(p, _)| p).as_ref() != Some(&journal.next) {
        bail!("checkpoint activation outcome unresolved");
    }
    while let Some(object) = journal.objects.last().cloned() {
        io.remove_object(&object)?;
        journal.objects.pop();
        save(&path, &journal)?;
    }
    while let Some(proposal) = journal.proposals.last().cloned() {
        io.remove(&proposal)?;
        journal.proposals.pop();
        save(&path, &journal)?;
    }
    if let Some(old) = &journal.expected {
        if old.hash != journal.next.hash {
            io.remove(&checkpoint_path(old))?;
        }
    }
    fs::remove_file(&path)?;
    #[cfg(unix)]
    File::open(local_root)?.sync_all()?;
    Ok((journal.next, journal.checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    #[derive(Default)]
    struct Memory {
        files: RefCell<BTreeMap<String, Vec<u8>>>,
        uncertain_pointer: Cell<bool>,
        fail_delete: Cell<bool>,
        deletes: RefCell<Vec<String>>,
    }
    impl CheckpointIo for Memory {
        fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.files.borrow().get(key).cloned())
        }
        fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
            self.files.borrow_mut().insert(key.into(), bytes.into());
            if key == "current.json" && self.uncertain_pointer.replace(false) {
                bail!("injected unknown write outcome");
            }
            Ok(())
        }
        fn list(&self, dir: &str) -> Result<Vec<String>> {
            Ok(self
                .files
                .borrow()
                .keys()
                .filter_map(|k| k.strip_prefix(&format!("{dir}/")).map(str::to_owned))
                .collect())
        }
        fn remove(&self, key: &str) -> Result<()> {
            if self.fail_delete.replace(false) {
                bail!("injected delete failure");
            }
            self.files.borrow_mut().remove(key);
            self.deletes.borrow_mut().push(key.into());
            Ok(())
        }
        fn remove_object(&self, key: &str) -> Result<()> {
            self.remove(key)
        }
        fn verify(&self, value: &ManagedContent) -> Result<()> {
            value.validate()
        }
    }
    fn owner() -> String {
        "a".repeat(64)
    }
    fn proposal(serial: u64) -> Proposal {
        Proposal {
            version: 5,
            id: "b".repeat(64),
            serial,
            device: "c".repeat(64),
            worker: "test".into(),
            path: "file".into(),
            base: None,
            value: None,
        }
    }
    #[test]
    fn pointer_unknown_outcome_resumes_forward() {
        let io = Memory::default();
        let dir = tempfile::tempdir().unwrap();
        io.uncertain_pointer.set(true);
        assert!(coordinate(
            &io,
            dir.path(),
            &owner(),
            1,
            Some(Checkpoint::empty(&owner())),
            None,
        )
        .is_err());
        let active = load(&io).unwrap().unwrap().0;
        assert!(dir
            .path()
            .join("shared-checkpoint-transition.json")
            .exists());
        let result = coordinate(&io, dir.path(), &owner(), 1, None, None).unwrap();
        assert_eq!(result.0, active);
        assert!(!dir
            .path()
            .join("shared-checkpoint-transition.json")
            .exists());
    }
    #[test]
    fn publication_is_not_acknowledgement_and_cleanup_resumes() {
        let io = Memory::default();
        let dir = tempfile::tempdir().unwrap();
        coordinate(
            &io,
            dir.path(),
            &owner(),
            1,
            Some(Checkpoint::empty(&owner())),
            None,
        )
        .unwrap();
        let p = proposal(0);
        publish_proposal(&io, &p).unwrap();
        assert!(!load(&io).unwrap().unwrap().1.receipts.contains(&p.id));
        io.fail_delete.set(true);
        assert!(coordinate(&io, dir.path(), &owner(), 1, None, None).is_err());
        assert!(load(&io).unwrap().unwrap().1.receipts.contains(&p.id));
        let result = coordinate(&io, dir.path(), &owner(), 1, None, None).unwrap();
        assert_eq!(result.0.serial, 1);
        assert!(result.1.receipts.contains(&p.id));
        assert!(io.list("proposals").unwrap().is_empty());
        // A stale request is cleanup-only, never newly acknowledged.
        let mut stale = proposal(0);
        stale.id = "d".repeat(64);
        publish_proposal(&io, &stale).unwrap();
        let (_, result) = coordinate(&io, dir.path(), &owner(), 1, None, None).unwrap();
        assert!(!result.receipts.contains(&stale.id));
        assert_eq!(result.serial, 2);
        let before = load(&io).unwrap().unwrap().0;
        let after = coordinate(&io, dir.path(), &owner(), 1, None, None)
            .unwrap()
            .0;
        assert_eq!(before, after);
    }
    #[test]
    fn owner_and_unrelated_pointer_are_rejected() {
        let io = Memory::default();
        let dir = tempfile::tempdir().unwrap();
        coordinate(
            &io,
            dir.path(),
            &owner(),
            1,
            Some(Checkpoint::empty(&owner())),
            None,
        )
        .unwrap();
        assert!(coordinate(&io, dir.path(), &"f".repeat(64), 1, None, None).is_err());
        publish_proposal(&io, &proposal(0)).unwrap();
        io.uncertain_pointer.set(true);
        assert!(coordinate(&io, dir.path(), &owner(), 1, None, None).is_err());
        let mut other = Checkpoint::empty(&owner());
        other.serial = 99;
        let bytes = serde_json::to_vec(&other).unwrap();
        let pointer = Pointer {
            version: 5,
            owner: owner(),
            serial: 99,
            hash: hash(&bytes),
        };
        io.put(&checkpoint_path(&pointer), &bytes).unwrap();
        io.put("current.json", &serde_json::to_vec(&pointer).unwrap())
            .unwrap();
        assert!(coordinate(&io, dir.path(), &owner(), 1, None, None).is_err());
        assert!(io.deletes.borrow().is_empty());
    }
    #[test]
    fn backward_pointer_inside_coordination_is_rejected_without_mutation() {
        let io = Memory::default();
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = coordinate(
            &io,
            dir.path(),
            &owner(),
            1,
            Some(Checkpoint::empty(&owner())),
            None,
        )
        .unwrap();
        let floor = super::super::namespace::CheckpointCursor {
            owner: p.owner,
            serial: p.serial + 1,
            hash: p.hash,
        };
        let before = io.files.borrow().clone();
        assert!(coordinate(&io, dir.path(), &owner(), 1, None, Some(&floor)).is_err());
        assert_eq!(*io.files.borrow(), before);
        assert!(io.deletes.borrow().is_empty());
    }
}
