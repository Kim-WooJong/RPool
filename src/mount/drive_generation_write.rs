//! Publishes the records that adopt migrated drive files into a new metadata
//! epoch (pool change migration, phase 3). v6: one parentless event per file
//! referencing its (migrated or unchanged) manifest. v7: one fresh snapshot
//! and name per file over a private `peer-v7-<owner>` payload. Records are
//! deterministic for a migration, write-once and verified by readback on
//! every replica (`SharedTransport::publish`), so publishing again (another
//! PC, a resumed adoption) is idempotent. Nothing existing is overwritten.
use super::peer_snapshot::migration as v7;
use super::peer_snapshot_transport::Store;
use super::shared_model::{Content, Event};
use super::shared_transport::SharedTransport;
use super::virtual_drive::drive_metadata_roots;
use crate::migration::drive_model::DriveFile;
use crate::prelude::*;

/// One record: `kind` is `events` (v6), `snapshots` or `names` (v7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub kind: &'static str,
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

/// The records that publish `files` in `epoch` of `pool` on `remotes` (the
/// new policy's remotes, as in the pool config). `history_limit` is the v7
/// snapshot policy limit; `seed` is the migration id.
pub(crate) fn records(
    pool: &str,
    remotes: &[String],
    epoch: &str,
    v7: bool,
    files: &[DriveFile],
    history_limit: usize,
    seed: &str,
) -> Result<Vec<Record>> {
    if v7 {
        let roots = drive_metadata_roots(pool, remotes, Some(epoch), true)?;
        let adopted = v7::adopted(files, &roots, history_limit, seed)?;
        // Snapshots first: a name is only valid once its snapshot is visible.
        let mut out: Vec<Record> = adopted
            .snapshots
            .into_iter()
            .map(|(id, bytes)| Record {
                kind: "snapshots",
                id,
                bytes,
            })
            .collect();
        out.extend(adopted.names.into_iter().map(|(id, bytes)| Record {
            kind: "names",
            id,
            bytes,
        }));
        return Ok(out);
    }
    let device = device(seed)?;
    let mut events = BTreeMap::new();
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let event = Event {
            version: 1,
            worker: v7::WORKER.into(),
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
            kind: "events",
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

/// Publishes `records` in order of kind (all snapshots before any name),
/// several at once within a kind.
pub(crate) fn publish(sink: &dyn Sink, records: &[Record]) -> Result<()> {
    for kind in ["events", "snapshots", "names"] {
        records
            .par_iter()
            .filter(|r| r.kind == kind)
            .try_for_each(|r| sink.publish(r))?;
    }
    Ok(())
}

/// The replicas of the new epoch on the pool's (new) remotes.
pub(crate) struct CloudSink {
    v6: Vec<SharedTransport>,
    v7: Option<Store>,
}

impl CloudSink {
    pub(crate) fn new(
        rclone: &str,
        pool: &str,
        policy: &PoolDefinition,
        epoch: &str,
        v7: bool,
    ) -> Result<Self> {
        let roots = drive_metadata_roots(pool, &policy.remotes, Some(epoch), v7)?;
        if v7 {
            return Ok(Self {
                v6: vec![],
                v7: Some(Store::new(rclone, &roots)?.with_native_crypt(policy.native_crypt)),
            });
        }
        Ok(Self {
            v6: roots
                .iter()
                .map(|root| {
                    SharedTransport::new(rclone, root)
                        .map(|t| t.with_native_crypt(policy.native_crypt))
                })
                .collect::<Result<_>>()?,
            v7: None,
        })
    }
}

impl Sink for CloudSink {
    fn publish(&self, record: &Record) -> Result<()> {
        match (&self.v7, record.kind) {
            (Some(store), "snapshots" | "names") => {
                store.publish(record.kind, &record.id, &record.bytes)
            }
            (None, "events") => {
                for transport in &self.v6 {
                    transport.publish(&record.id, &record.bytes)?;
                }
                Ok(())
            }
            _ => bail!("record kind {} does not match this generation", record.kind),
        }
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
        let files = vec![file("a/one.txt", "migrate-1"), file("two.txt", "virtual-2")];
        let remotes = vec!["a:".to_string()];
        let records = records("pool", &remotes, &"e".repeat(64), false, &files, 0, "m1").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records,
            records_again(&files, &remotes),
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
        assert_eq!(seen["a/one.txt"], "migrate-1");
        assert_eq!(seen["two.txt"], "virtual-2");
    }

    fn records_again(files: &[DriveFile], remotes: &[String]) -> Vec<Record> {
        records("pool", remotes, &"e".repeat(64), false, files, 0, "m1").unwrap()
    }

    #[test]
    fn colliding_paths_are_refused() {
        let files = vec![file("Same.txt", "x1"), file("same.txt", "x2")];
        assert!(records(
            "pool",
            &["a:".into()],
            &"e".repeat(64),
            false,
            &files,
            0,
            "m"
        )
        .is_err());
    }

    struct Seen(Mutex<Vec<&'static str>>);
    impl Sink for Seen {
        fn publish(&self, record: &Record) -> Result<()> {
            self.0.lock().unwrap().push(record.kind);
            Ok(())
        }
    }

    #[test]
    fn snapshots_are_published_before_names() {
        let mk = |kind, id: &str| Record {
            kind,
            id: id.into(),
            bytes: vec![],
        };
        let records = vec![
            mk("names", "n1"),
            mk("snapshots", "s1"),
            mk("names", "n2"),
            mk("snapshots", "s2"),
        ];
        let sink = Seen(Mutex::new(vec![]));
        publish(&sink, &records).unwrap();
        assert_eq!(
            *sink.0.lock().unwrap(),
            vec!["snapshots", "snapshots", "names", "names"]
        );
    }
}
