//! v7 side of a pool change migration (phase 3): read a generation's drive
//! straight from its records, and build the records that adopt migrated
//! files into a new epoch. A child of `peer_snapshot` for its private record
//! types; it never changes how a mount syncs.
//!
//! Reading cannot use a mount's own validation: a generation's snapshot
//! policy (genesis) is derived from the metadata roots of the remote set it
//! was written with, and its payloads may sit on accounts that left the
//! pool. The records' own (single) policy is analysed instead, and a scratch
//! workspace materializes the view exactly as a mount would name it.
//!
//! Adopted records are deterministic for a migration (file ids, name nonces
//! and the device label derive from the migration id; owners come from the
//! migrated `peer-v7-<owner>` payloads), so PCs adopting the same migration
//! publish identical records.
use super::model::{self, Policy, Revision, Snapshot};
use super::{hash, validate_names, NameOp, State};
use crate::migration::drive_model::DriveFile;
use crate::mount::peer_snapshot_transport::Store;
use crate::mount::virtual_drive::{Revision as DriveRevision, VirtualDrive};
use crate::prelude::*;

/// Worker label of adopted revisions.
pub(crate) const WORKER: &str = "pool-migration";

/// Records of one v7 generation, by kind: id -> JSON bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Records {
    pub snapshots: BTreeMap<String, Vec<u8>>,
    pub names: BTreeMap<String, Vec<u8>>,
}

/// Lists and reads every record of the generation at `roots` (read-only).
pub(crate) fn collect(rclone: &str, roots: &[String], native_crypt: bool) -> Result<Records> {
    use crate::mount::metadata_checkpoint_model::Family;
    let store = Store::new(rclone, roots)?.with_native_crypt(native_crypt);
    let gate = store.gate_listed()?;
    let mut records = Records {
        snapshots: store.collect("snapshots", &BTreeSet::new())?,
        names: store.collect("names", &BTreeSet::new())?,
    };
    // Records kept only in checkpoints (their own objects may be compacted
    // away) belong to the generation too.
    let family = Family::v7();
    let dirs = crate::mount::metadata_pool::replica_dirs(rclone, roots, native_crypt, &family)?;
    let replicas: Vec<_> = dirs.iter().map(|d| d.replica()).collect();
    crate::mount::metadata_checkpoint::pull(
        &family,
        &replicas,
        &mut crate::mount::metadata_cache::Cache::default(),
        gate,
        true,
        &mut |kind, id, text| {
            let target = match kind {
                "snapshots" => &mut records.snapshots,
                "names" => &mut records.names,
                _ => return Ok(()),
            };
            target
                .entry(id.to_owned())
                .or_insert_with(|| text.as_bytes().to_vec());
            Ok(())
        },
    )?;
    Ok(records)
}

fn state_of(records: &Records) -> Result<State> {
    let mut state = State::default();
    for (id, bytes) in &records.snapshots {
        let snapshot: Snapshot = serde_json::from_slice(bytes)?;
        if snapshot.id()? != *id {
            bail!("v7 snapshot identity mismatch");
        }
        state.snapshots.insert(id.clone(), snapshot);
    }
    for (id, bytes) in &records.names {
        let op: NameOp = serde_json::from_slice(bytes)?;
        if hash(&op)? != *id {
            bail!("v7 name identity mismatch");
        }
        state.names.insert(id.clone(), op);
    }
    Ok(state)
}

/// The single snapshot policy of a generation's records.
fn policy_of(state: &State) -> Result<Option<Policy>> {
    let mut found: Option<Policy> = None;
    for snapshot in state.snapshots.values() {
        match &found {
            Some(policy) if *policy != snapshot.policy => {
                bail!("v7 records of one generation carry different snapshot policies")
            }
            Some(_) => {}
            None => found = Some(snapshot.policy.clone()),
        }
    }
    Ok(found)
}

/// The files a mount of `records` shows, and the recorded history limit.
/// `scratch` is a throwaway v7 workspace; only its local state is written.
pub(crate) fn view(
    scratch: &VirtualDrive,
    records: &Records,
) -> Result<(Vec<DriveFile>, Option<usize>)> {
    if !scratch.peer_retention {
        bail!("v7 view needs a v7 scratch workspace");
    }
    let mut state = state_of(records)?;
    let Some(policy) = policy_of(&state)? else {
        return Ok((vec![], None));
    };
    let analysis = model::analyze(&state.snapshots, &policy)?;
    validate_names(&state)?;
    scratch.materialize_snapshots(&mut state, &analysis)?;
    let mut files = Vec::new();
    for (path, revision) in scratch.view()? {
        if let DriveRevision::Cloud { id, content } = revision {
            files.push(DriveFile {
                path,
                revision: id,
                hash: content.hash,
                size: content.size,
                manifest: content.manifest,
            });
        }
    }
    Ok((files, Some(policy.history_limit)))
}

/// Records that publish `files` (whose manifests are migrated
/// `peer-v7-<owner>` payloads, one owner per file) as fresh files of the
/// generation at `roots` (the v7 roots a mount of that epoch uses).
pub(crate) fn adopted(
    files: &[DriveFile],
    roots: &[String],
    history_limit: usize,
    seed: &str,
) -> Result<Records> {
    let policy = Policy {
        genesis_id: hash(&("rpool-private-v7", roots))?,
        history_limit,
    };
    let device = hash(&("rpool-migration-device", seed))?;
    let mut state = State::default();
    let mut records = Records::default();
    for file in files {
        let owner = file
            .manifest
            .archive_id
            .strip_prefix("peer-v7-")
            .filter(|o| o.len() == 64 && o.bytes().all(|b| b.is_ascii_hexdigit()))
            .with_context(|| format!("{}: payload is not a private v7 copy", file.path))?
            .to_owned();
        if file.manifest.original_size != file.size {
            bail!("{}: payload size differs from the file", file.path);
        }
        let file_id = hash(&("rpool-migration-v7-file", seed, &file.path))?;
        let revision = Revision {
            parents: BTreeSet::new(),
            content_hash: Some(file.hash.clone()),
            size: file.size,
            worker: WORKER.into(),
            device: device.clone(),
        };
        let revision_id = revision.id()?;
        let snapshot = model::build(
            file_id.clone(),
            owner,
            policy.clone(),
            BTreeSet::new(),
            &BTreeMap::new(),
            BTreeMap::from([(revision_id.clone(), revision)]),
            BTreeMap::from([(revision_id, file.manifest.clone())]),
        )?;
        let id = snapshot.id()?;
        records
            .snapshots
            .insert(id.clone(), serde_json::to_vec(&snapshot)?);
        state.snapshots.insert(id, snapshot);
        let op = NameOp {
            nonce: hash(&("rpool-migration-v7-name", seed, &file.path))?,
            entries: BTreeMap::from([(file_id.clone(), file.path.clone())]),
            parents: BTreeMap::from([(file_id, BTreeSet::new())]),
        };
        let name = hash(&op)?;
        records.names.insert(name.clone(), serde_json::to_vec(&op)?);
        state.names.insert(name, op);
    }
    // The whole set must be what a mount accepts (owners and objects unique).
    if !state.snapshots.is_empty() {
        model::analyze(&state.snapshots, &policy)?;
    }
    validate_names(&state)?;
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::test_support::manifest;

    fn v7_scratch(dir: &Path, roots: &[String]) -> VirtualDrive {
        let mut drive = crate::mount::virtual_drive::fixture(dir);
        drive.peer_retention = true;
        drive.pool_sync_roots = roots.to_vec();
        drive.state.lock().unwrap().version = 7;
        drive
    }

    fn file(path: &str, owner: char, size: u64) -> DriveFile {
        let mut m = manifest(
            &format!("peer-v7-{}", owner.to_string().repeat(64)),
            size,
            1048576,
            2,
            1,
            &["a:", "b:", "c:"],
        );
        m.original_name = path.into();
        DriveFile {
            path: path.into(),
            revision: String::new(),
            hash: blake3::hash(path.as_bytes()).to_hex().to_string(),
            size,
            manifest: m,
        }
    }

    #[test]
    fn adopted_records_materialize_as_the_same_files_on_a_fresh_reader() {
        let roots = vec!["a:x/snapshots-v7/s/epochs/e".to_string()];
        let files = vec![file("docs/a.txt", 'a', 3), file("b.bin", 'b', 2 * 1048576)];
        let records = adopted(&files, &roots, 2, "migration-1").unwrap();
        assert_eq!(records.snapshots.len(), 2);
        assert_eq!(records.names.len(), 2);
        // Deterministic: another PC adopting the same migration agrees.
        assert_eq!(adopted(&files, &roots, 2, "migration-1").unwrap(), records);
        let dir = tempfile::tempdir().unwrap();
        let scratch = v7_scratch(dir.path(), &roots);
        let (seen, limit) = view(&scratch, &records).unwrap();
        assert_eq!(limit, Some(2));
        let mut paths: Vec<_> = seen
            .iter()
            .map(|f| (f.path.clone(), f.hash.clone(), f.size))
            .collect();
        paths.sort();
        let mut want: Vec<_> = files
            .iter()
            .map(|f| (f.path.clone(), f.hash.clone(), f.size))
            .collect();
        want.sort();
        assert_eq!(paths, want);
        for f in &seen {
            assert!(f.manifest.archive_id.starts_with("peer-v7-"));
        }
        // A mount of that epoch derives the same genesis from its roots.
        let state = state_of(&records).unwrap();
        let mount_policy = Policy {
            genesis_id: hash(&("rpool-private-v7", &roots)).unwrap(),
            history_limit: 2,
        };
        model::analyze(&state.snapshots, &mount_policy).unwrap();
    }

    #[test]
    fn shared_owner_or_foreign_payload_is_refused() {
        let roots = vec!["a:x".to_string()];
        let mut foreign = file("x", 'a', 3);
        foreign.manifest.archive_id = "virtual-abc".into();
        assert!(adopted(&[foreign], &roots, 0, "m").is_err());
        let twins = [file("x", 'a', 3), file("y", 'a', 3)];
        assert!(adopted(&twins, &roots, 0, "m").is_err());
    }

    #[test]
    fn empty_generation_has_no_files_and_no_policy() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = v7_scratch(dir.path(), &["a:x".to_string()]);
        let (files, limit) = view(&scratch, &Records::default()).unwrap();
        assert!(files.is_empty() && limit.is_none());
    }
}
