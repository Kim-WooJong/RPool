//! Immutable v7 records and exact private-payload reclamation. Immutable records
//! are never deleted; optional archive-manifest deletion uses exact owned paths.
use super::shared_transport::SharedTransport;
use crate::prelude::*;
use crate::storage::{
    error::{StorageError, StorageErrorKind},
    writer::StorageWriter,
};

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::NotFound)
}
fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

pub(crate) struct Store {
    rclone: String,
    roots: Vec<String>,
    native_crypt: bool,
}
impl Store {
    pub(crate) fn new(rclone: &str, roots: &[String]) -> Result<Self> {
        if roots.is_empty() {
            bail!("peer metadata roots missing");
        }
        for root in roots {
            SharedTransport::new(rclone, root)?;
        }
        Ok(Self {
            rclone: rclone.into(),
            roots: roots.to_vec(),
            native_crypt: false,
        })
    }
    /// Snapshot records and private copies in a native-crypt pool.
    pub(crate) fn with_native_crypt(mut self, native_crypt: bool) -> Self {
        self.native_crypt = native_crypt;
        self
    }
    fn transports(&self, kind: &str) -> Result<Vec<SharedTransport>> {
        if !matches!(kind, "snapshots" | "retirements" | "names") {
            bail!("invalid peer record kind");
        }
        self.roots
            .iter()
            .map(|root| {
                SharedTransport::new(&self.rclone, &crate::utils::remote_join(root, kind))
                    .map(|t| t.with_native_crypt(self.native_crypt))
            })
            .collect()
    }
    pub(crate) fn collect(
        &self,
        kind: &str,
        known: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();
        let mut size = 0usize;
        for transport in self.transports(kind)? {
            for (id, bytes) in transport.list_missing(known)? {
                if let Some(previous) = result.get(&id) {
                    if previous != &bytes {
                        bail!("peer record replica mismatch");
                    }
                } else {
                    size = size
                        .checked_add(bytes.len())
                        .context("peer metadata overflow")?;
                    if result.len() >= 10_000 || size > 64 * 1024 * 1024 {
                        bail!("peer metadata bootstrap limit exceeded");
                    }
                    result.insert(id, bytes);
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn publish(&self, kind: &str, id: &str, bytes: &[u8]) -> Result<()> {
        for transport in self.transports(kind)? {
            transport.publish(id, bytes)?;
        }
        Ok(())
    }
    pub(crate) fn copy_revision(
        &self,
        owner: &str,
        revision: &str,
        manifest: &Manifest,
    ) -> Result<Manifest> {
        copy_private_key(
            &StorageWriter::for_pool(&self.rclone, self.native_crypt),
            owner,
            revision,
            manifest,
        )
    }
    pub(crate) fn resume_gc(
        &self,
        path: &Path,
        proof: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        resume_gc_with(
            &StorageWriter::for_pool(&self.rclone, self.native_crypt),
            path,
            proof,
        )
    }
}

/// A deliberately narrow injectable boundary: callers cannot delete a prefix.
pub(crate) trait PayloadIo {
    fn verify_if_present(&self, shard: &Shard) -> Result<bool>;
    fn copy(&self, source: &Shard, target: &Shard) -> Result<()>;
    fn remove(&self, object: &str) -> Result<()>;
}
impl PayloadIo for StorageWriter {
    fn verify_if_present(&self, shard: &Shard) -> Result<bool> {
        self.ensure_destination(&shard.object)?;
        match self.reader().stat(&shard.object) {
            Err(e) if missing(&e) => Ok(false),
            Err(e) => Err(e),
            Ok(_) => {
                self.reader().verify(shard, true)?;
                Ok(true)
            }
        }
    }
    fn copy(&self, source: &Shard, target: &Shard) -> Result<()> {
        self.ensure_destination(&source.object)?;
        self.copy_verified(source, &target.object, 1)?;
        self.reader().verify(target, true)
    }
    fn remove(&self, object: &str) -> Result<()> {
        self.ensure_destination(object)?;
        match self.delete(object) {
            Err(e) if missing(&e) => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
pub(crate) fn copy_private_with(
    io: &impl PayloadIo,
    owner: &str,
    original: &Manifest,
) -> Result<Manifest> {
    copy_private_key(
        io,
        owner,
        &crate::manifest::manifest_fingerprint(original)?,
        original,
    )
}
pub(crate) fn copy_private_key(
    io: &impl PayloadIo,
    owner: &str,
    fingerprint: &str,
    original: &Manifest,
) -> Result<Manifest> {
    if !valid_id(owner) || !valid_id(fingerprint) {
        bail!("invalid private payload owner");
    }
    crate::manifest::validate_manifest(original)?;
    let mut result = original.clone();
    result.version = 2;
    result.archive_id = format!("peer-v7-{owner}");
    for (source, target) in original.shards.iter().zip(result.shards.iter_mut()) {
        SharedTransport::new("unused", &source.remote)?;
        target.object = crate::utils::remote_join(
            &source.remote,
            &format!("peer-v7-{owner}/{fingerprint}/shard-{}", source.index),
        );
        // Existing private objects are immutable. Corruption is an error, never
        // permission to overwrite objects a published snapshot might reference.
        if !io.verify_if_present(target)? {
            io.copy(source, target)?;
        }
        if !io.verify_if_present(target)? {
            bail!("private copy missing after write");
        }
    }
    result.content_root_blake3 = crate::manifest::content_root_v2(
        result.original_size,
        result.shard_size,
        &result.coding,
        &result.shards,
    );
    crate::manifest::validate_manifest(&result)?;
    Ok(result)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GcJournal {
    pub predecessor: String,
    pub successor: String,
    pub owner: String,
    pub objects: Vec<String>,
    remaining: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct CheckedJournal {
    hash: String,
    value: GcJournal,
}
impl GcJournal {
    /// Caller must prove these manifests belong to the obsolete predecessor.
    /// Resume additionally demands a fresh successor/dominance proof before
    /// every mutation. Merely writing a journal never grants that authority.
    #[cfg(test)]
    pub(crate) fn prepare(
        path: &Path,
        predecessor: &str,
        successor: &str,
        owner: &str,
        manifests: &[Manifest],
    ) -> Result<()> {
        Self::prepare_with_metadata(path, predecessor, successor, owner, manifests, &[])
    }
    pub(crate) fn prepare_with_metadata(
        path: &Path,
        predecessor: &str,
        successor: &str,
        owner: &str,
        manifests: &[Manifest],
        metadata_objects: &[String],
    ) -> Result<()> {
        if !valid_id(predecessor)
            || !valid_id(successor)
            || predecessor == successor
            || !valid_id(owner)
        {
            bail!("invalid peer GC proof identities");
        }
        let mut objects = BTreeSet::new();
        let mut allowed_metadata = BTreeSet::new();
        for manifest in manifests {
            crate::manifest::validate_manifest(manifest)?;
            if manifest.archive_id != format!("peer-v7-{owner}") {
                bail!("GC manifest not privately owned");
            }
            for shard in &manifest.shards {
                SharedTransport::new("unused", &shard.remote)?;
                let prefix = crate::utils::remote_join(&shard.remote, &format!("peer-v7-{owner}/"));
                let suffix = shard
                    .object
                    .strip_prefix(&prefix)
                    .context("GC object outside private owner")?;
                if suffix.is_empty()
                    || suffix.contains([':', '\\'])
                    || suffix.chars().any(char::is_control)
                    || suffix
                        .split('/')
                        .any(|s| s.is_empty() || matches!(s, "." | ".."))
                    || suffix == "manifest.json"
                {
                    bail!("GC object is not an exact private shard");
                }
                objects.insert(shard.object.clone());
                allowed_metadata.insert(crate::utils::remote_join(
                    &shard.remote,
                    &format!("peer-v7-{owner}/manifest.json"),
                ));
            }
        }
        for object in metadata_objects {
            if !allowed_metadata.contains(object) {
                bail!("GC metadata is not an owned archive manifest");
            }
            objects.insert(object.clone());
        }
        let objects: Vec<_> = objects.into_iter().collect();
        let journal = Self {
            predecessor: predecessor.into(),
            successor: successor.into(),
            owner: owner.into(),
            remaining: objects.clone(),
            objects,
        };
        if path.exists() {
            let old = load_journal(path)?;
            if old.predecessor != journal.predecessor
                || old.successor != journal.successor
                || old.owner != journal.owner
                || old.objects != journal.objects
            {
                bail!("another peer GC operation is pending");
            }
            return Ok(());
        }
        save_journal(path, &journal)
    }
}
fn save_journal(path: &Path, value: &GcJournal) -> Result<()> {
    super::namespace::durable_json(
        path,
        &CheckedJournal {
            hash: digest(value)?,
            value: value.clone(),
        },
    )
}
fn load_journal(path: &Path) -> Result<GcJournal> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("invalid GC journal file");
    }
    let checked: CheckedJournal = crate::utils::read_json(path)?;
    if digest(&checked.value)? != checked.hash {
        bail!("peer GC journal checksum mismatch");
    }
    let value = checked.value;
    if !valid_id(&value.predecessor)
        || !valid_id(&value.successor)
        || value.predecessor == value.successor
        || !valid_id(&value.owner)
        || value.remaining.iter().any(|o| !value.objects.contains(o))
    {
        bail!("invalid GC journal");
    }
    Ok(value)
}
pub(crate) fn resume_gc_with(
    io: &impl PayloadIo,
    path: &Path,
    mut proof: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let mut journal = load_journal(path)?;
    while let Some(object) = journal.remaining.first().cloned() {
        proof(&journal.predecessor, &journal.successor)?;
        io.remove(&object)?;
        journal.remaining.remove(0);
        save_journal(path, &journal)?;
    }
    // Keep the completed receipt. A caller can archive it locally once it has
    // recorded completion; remote metadata is never deleted here.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    #[derive(Default)]
    struct Io {
        objects: RefCell<BTreeMap<String, String>>,
        copies: Cell<usize>,
        deletes: Cell<usize>,
        fail: Cell<bool>,
    }
    impl PayloadIo for Io {
        fn verify_if_present(&self, shard: &Shard) -> Result<bool> {
            match self.objects.borrow().get(&shard.object) {
                None => Ok(false),
                Some(hash) if hash == &shard.blake3 => Ok(true),
                _ => bail!("corrupt"),
            }
        }
        fn copy(&self, source: &Shard, target: &Shard) -> Result<()> {
            if !self.verify_if_present(source)? {
                bail!("source missing");
            }
            self.objects
                .borrow_mut()
                .insert(target.object.clone(), source.blake3.clone());
            self.copies.set(self.copies.get() + 1);
            Ok(())
        }
        fn remove(&self, object: &str) -> Result<()> {
            self.deletes.set(self.deletes.get() + 1);
            if self.fail.replace(false) {
                bail!("injected remote failure");
            }
            self.objects.borrow_mut().remove(object);
            Ok(())
        }
    }
    fn manifest() -> Manifest {
        let shards = vec![Shard {
            index: 0,
            offset: 0,
            size: 1,
            remote: "crypt:".into(),
            object: "crypt:old/shard".into(),
            blake3: blake3::hash(b"a").to_hex().to_string(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        }];
        Manifest {
            version: 2,
            archive_id: "old".into(),
            original_name: "file".into(),
            original_size: 1,
            shard_size: 1,
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(1, 1, &None, &shards),
            coding: None,
            shards,
        }
    }
    #[test]
    fn private_copy_retries_and_gc_resume_preserve_source_and_require_proof() {
        let io = Io::default();
        let source = manifest();
        io.objects.borrow_mut().insert(
            source.shards[0].object.clone(),
            source.shards[0].blake3.clone(),
        );
        let owner = "a".repeat(64);
        let private = copy_private_with(&io, &owner, &source).unwrap();
        copy_private_with(&io, &owner, &source).unwrap();
        assert_eq!(io.copies.get(), 1);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gc.json");
        GcJournal::prepare(
            &path,
            &"b".repeat(64),
            &"c".repeat(64),
            &owner,
            &[private.clone()],
        )
        .unwrap();
        assert!(resume_gc_with(&io, &path, |_, _| bail!("successor unavailable")).is_err());
        assert_eq!(io.deletes.get(), 0);
        io.fail.set(true);
        assert!(resume_gc_with(&io, &path, |_, _| Ok(())).is_err());
        resume_gc_with(&io, &path, |_, _| Ok(())).unwrap();
        resume_gc_with(&io, &path, |_, _| Ok(())).unwrap();
        assert_eq!(io.deletes.get(), 2);
        assert!(io.objects.borrow().contains_key(&source.shards[0].object));
        assert!(!io.objects.borrow().contains_key(&private.shards[0].object));
    }
    #[test]
    fn missing_payload_is_idempotent_and_proof_is_rechecked_for_each_object() {
        let io = Io::default();
        let source = manifest();
        let owner = "a".repeat(64);
        io.objects.borrow_mut().insert(
            source.shards[0].object.clone(),
            source.shards[0].blake3.clone(),
        );
        let private = copy_private_with(&io, &owner, &source).unwrap();
        let metadata = format!("crypt:peer-v7-{owner}/manifest.json");
        io.objects
            .borrow_mut()
            .insert(metadata.clone(), "metadata".into());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gc.json");
        GcJournal::prepare_with_metadata(
            &path,
            &"b".repeat(64),
            &"c".repeat(64),
            &owner,
            &[private.clone()],
            &[metadata.clone()],
        )
        .unwrap();
        io.objects.borrow_mut().remove(&private.shards[0].object);
        let mut proofs = 0;
        assert!(resume_gc_with(&io, &path, |_, _| {
            proofs += 1;
            if proofs == 2 {
                bail!("successor proof temporarily unavailable");
            }
            Ok(())
        })
        .is_err());
        assert_eq!(io.deletes.get(), 1);
        assert!(io.objects.borrow().contains_key(&metadata));
        resume_gc_with(&io, &path, |_, _| Ok(())).unwrap();
        assert!(!io.objects.borrow().contains_key(&metadata));
        assert!(io.objects.borrow().contains_key(&source.shards[0].object));
    }
    #[test]
    fn foreign_and_metadata_deletion_rejected_and_existing_corruption_not_overwritten() {
        let io = Io::default();
        let source = manifest();
        let owner = "a".repeat(64);
        io.objects.borrow_mut().insert(
            source.shards[0].object.clone(),
            source.shards[0].blake3.clone(),
        );
        let mut private = copy_private_with(&io, &owner, &source).unwrap();
        io.objects
            .borrow_mut()
            .insert(private.shards[0].object.clone(), "bad".into());
        assert!(copy_private_with(&io, &owner, &source).is_err());
        assert_eq!(io.copies.get(), 1);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gc.json");
        assert!(
            GcJournal::prepare(&path, &"b".repeat(64), &"c".repeat(64), &owner, &[source]).is_err()
        );
        private.shards[0].object = format!("crypt:peer-v7-{owner}/manifest.json");
        private.content_root_blake3 =
            crate::manifest::content_root_v2(1, 1, &None, &private.shards);
        assert!(
            GcJournal::prepare(&path, &"b".repeat(64), &"c".repeat(64), &owner, &[private])
                .is_err()
        );
    }
}
