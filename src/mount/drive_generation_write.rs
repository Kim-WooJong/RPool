//! Publishes the records that adopt migrated drive files into a new metadata
//! epoch (pool change migration, phase 3): one parentless event per file
//! referencing its (migrated or unchanged) manifest. Records are
//! deterministic for a migration, write-once and verified by readback on
//! every replica (`SharedTransport::publish`), so publishing again (another
//! PC, a resumed adoption) is idempotent. Nothing existing is overwritten.
use super::shared_model::{Content, Event};
use super::shared_transport::SharedTransport;
use super::virtual_drive::drive_metadata_roots;
use crate::migration::drive_model::DriveFile;
use crate::prelude::*;

/// Worker name of adopted events.
const WORKER: &str = "pool-migration";

/// One v6 event record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub id: String,
    pub bytes: Vec<u8>,
}

/// Where adopted records go (every replica of the new epoch).
pub(crate) trait Sink: Sync {
    fn publish(&self, record: &Record) -> Result<()>;
}

fn device(seed: &str) -> Result<String> {
    Ok(
        blake3::hash(&serde_json::to_vec(&("rpool-migration-device", seed))?)
            .to_hex()
            .to_string(),
    )
}

/// The records that publish `files` in a new epoch; `seed` is the
/// migration id. They do not depend on the epoch or the pool's remotes (the
/// [`Sink`] places them).
pub(crate) fn records(files: &[DriveFile], seed: &str) -> Result<Vec<Record>> {
    let device = device(seed)?;
    let mut events = BTreeMap::new();
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let event = Event {
            version: 1,
            worker: WORKER.into(),
            device: device.clone(),
            path: file.path.clone(),
            parents: vec![],
            content: Some(Content {
                hash: file.hash.clone(),
                size: file.size,
                manifest: file.manifest.clone(),
            }),
        };
        event.validate()?;
        let id = event.id()?;
        out.push(Record {
            id: id.clone(),
            bytes: serde_json::to_vec(&event)?,
        });
        events.insert(id, event);
    }
    // The set must project cleanly (no case or directory collisions).
    if !events.is_empty() {
        super::peer_projection::project(&events)?;
    }
    Ok(out)
}

/// Publishes `records`, several at once.
pub(crate) fn publish(sink: &dyn Sink, records: &[Record]) -> Result<()> {
    records.par_iter().try_for_each(|r| sink.publish(r))
}

/// The replicas of the new epoch on the pool's (new) remotes.
pub(crate) struct CloudSink {
    v6: Vec<SharedTransport>,
}

impl CloudSink {
    pub(crate) fn new(
        rclone: &str,
        pool: &str,
        policy: &PoolDefinition,
        epoch: &str,
    ) -> Result<Self> {
        let roots = drive_metadata_roots(pool, &policy.remotes, Some(epoch))?;
        Ok(Self {
            v6: roots
                .iter()
                .map(|root| {
                    SharedTransport::new(rclone, root)
                        .map(|t| t.with_native_crypt(policy.native_crypt))
                })
                .collect::<Result<_>>()?,
        })
    }
}

impl Sink for CloudSink {
    fn publish(&self, record: &Record) -> Result<()> {
        for transport in &self.v6 {
            transport.publish(&record.id, &record.bytes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::test_support::manifest;

    fn file(path: &str, id: &str) -> DriveFile {
        DriveFile {
            path: path.into(),
            revision: String::new(),
            hash: blake3::hash(path.as_bytes()).to_hex().to_string(),
            size: 5,
            manifest: manifest(id, 5, 1048576, 2, 1, &["a:", "b:", "c:"]),
        }
    }

    #[test]
    fn v6_records_project_to_the_adopted_files_and_are_deterministic() {
        let files = vec![file("a/one.txt", "virtual-1"), file("two.txt", "virtual-2")];
        let records = records(&files, "m1").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records,
            super::records(&files, "m1").unwrap(),
            "another PC publishes the same bytes"
        );
        let events: BTreeMap<String, Event> = records
            .iter()
            .map(|r| (r.id.clone(), serde_json::from_slice(&r.bytes).unwrap()))
            .collect();
        for r in &records {
            assert_eq!(blake3::hash(&r.bytes).to_hex().as_str(), r.id);
        }
        let view = super::super::drive_generation_read::v6_view(&events).unwrap();
        let seen: BTreeMap<_, _> = view
            .iter()
            .map(|f| (f.path.clone(), f.manifest.archive_id.clone()))
            .collect();
        assert_eq!(seen["a/one.txt"], "virtual-1");
        assert_eq!(seen["two.txt"], "virtual-2");
    }

    #[test]
    fn colliding_paths_are_refused() {
        let files = vec![file("Same.txt", "x1"), file("same.txt", "x2")];
        assert!(records(&files, "m").is_err());
    }
}
