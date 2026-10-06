//! Copies a drive generation's metadata to accounts newly added to the pool,
//! so they can join as full metadata replicas without moving any file data.
//!
//! Every metadata object is immutable and named by the BLAKE3 hash of its
//! bytes, and `publish` of identical bytes is a no-op, so copying is
//! idempotent: an interrupted copy is simply run again, and two PCs copying
//! at once write the same objects. Readers merge every replica, so a
//! partially copied account never hides anything.
//!
//! Order per new account: checkpoint chunks, heads and marks, then the record
//! directories, and the compaction gate record last (deletion of covered
//! records waits for the gate on every replica). Each directory is listed
//! again afterwards: an object some old replica still lists must now be on
//! the new one, otherwise the copy fails and the caller keeps the old
//! membership. Objects that vanished meanwhile (deleted by compaction on
//! another PC) are skipped. Used by `VirtualDrive::open_internal`.
use super::metadata_checkpoint::list_all;
use super::metadata_dir::ObjectDir;
use crate::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Objects copied at the same time to one directory.
const COPY_PARALLEL: usize = 8;

/// The directories of one replica in copy order (see the module docs).
pub(crate) struct ReplicaView<'a, D> {
    /// Checkpoint chunk objects.
    pub chunks: &'a D,
    /// Checkpoint head objects.
    pub heads: &'a D,
    /// Deletion mark objects.
    pub marks: &'a D,
    /// Record directories (v6: one, `events`, which also holds the gate).
    pub records: Vec<&'a D>,
}

/// Copies every object of `sources` that `target` lacks; returns how many
/// objects were written. `gate` is held back until everything else is there.
pub(crate) fn backfill<D: ObjectDir + Sync>(
    sources: &[ReplicaView<'_, D>],
    target: &ReplicaView<'_, D>,
    gate: &str,
) -> Result<usize> {
    let mut copied = 0;
    let fixed = [
        (
            "checkpoint chunks",
            sources.iter().map(|r| r.chunks).collect::<Vec<_>>(),
            target.chunks,
        ),
        (
            "checkpoint heads",
            sources.iter().map(|r| r.heads).collect(),
            target.heads,
        ),
        (
            "checkpoint marks",
            sources.iter().map(|r| r.marks).collect(),
            target.marks,
        ),
    ];
    for (what, from, to) in fixed {
        copied += copy_dir(what, &from, to, &|_| true)?;
    }
    for (index, own) in target.records.iter().enumerate() {
        let from: Vec<&D> = sources
            .iter()
            .map(|r| r.records.get(index).copied())
            .collect::<Option<_>>()
            .context("metadata replicas have different record directories")?;
        copied += copy_dir("records", &from, own, &|id| id != gate)?;
    }
    if let Some(own) = target.records.first() {
        let from: Vec<&D> = sources
            .iter()
            .filter_map(|r| r.records.first().copied())
            .collect();
        copied += copy_dir("compaction gate", &from, own, &|id| id == gate)?;
    }
    Ok(copied)
}

/// Copies the objects of `from` selected by `wanted` that `to` lacks, then
/// verifies `to` holds every one still listed by some source.
fn copy_dir<D: ObjectDir + Sync>(
    what: &str,
    from: &[&D],
    to: &D,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<usize> {
    let dyn_from: Vec<&dyn ObjectDir> = from.iter().map(|d| *d as &dyn ObjectDir).collect();
    let listing = list_all(&dyn_from)?;
    let present: BTreeSet<String> = to.list()?.into_iter().map(|(id, _)| id).collect();
    let missing: Vec<(&String, u64, Vec<usize>)> = listing
        .iter()
        .filter(|(id, _)| wanted(id) && !present.contains(*id))
        .map(|(id, (size, holders))| (id, *size, holders.iter().copied().collect()))
        .collect();
    let next = AtomicUsize::new(0);
    let failures: Vec<(String, anyhow::Error)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..COPY_PARALLEL.min(missing.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut failed = Vec::new();
                    loop {
                        let Some((id, size, holders)) =
                            missing.get(next.fetch_add(1, Ordering::Relaxed))
                        else {
                            break failed;
                        };
                        let mut last = None;
                        for holder in holders {
                            match from[*holder]
                                .read(id, *size)
                                .and_then(|bytes| to.publish(id, &bytes))
                            {
                                Ok(()) => {
                                    last = None;
                                    break;
                                }
                                Err(error) => last = Some(error),
                            }
                        }
                        if let Some(error) = last {
                            failed.push(((*id).clone(), error));
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    // Verify: everything a source still lists must be on `to` now.
    let now: BTreeSet<String> = to.list()?.into_iter().map(|(id, _)| id).collect();
    let still = list_all(&dyn_from)?;
    let lacking: Vec<&String> = still
        .keys()
        .filter(|id| wanted(id) && !now.contains(*id))
        .collect();
    if let Some(id) = lacking.first() {
        let reason = failures
            .iter()
            .find(|(failed, _)| failed == *id)
            .map_or_else(|| "not written".to_owned(), |(_, e)| format!("{e:#}"));
        bail!(
            "copying {what} to the new account failed for {} object(s) (first {}: {reason}); the pool keeps its previous accounts",
            lacking.len(),
            &id[..12.min(id.len())]
        );
    }
    Ok(missing.len() - failures.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::metadata_dir::object_id;
    use std::sync::Mutex;

    /// In-memory directory; `fail_reads` makes every read fail.
    #[derive(Default)]
    struct Dir {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        fail_reads: bool,
        /// Ids removed right after the first listing (concurrent compaction).
        vanish: Mutex<Vec<String>>,
        /// Order of publications.
        log: Mutex<Vec<String>>,
    }
    impl Dir {
        fn with(objects: &[&[u8]]) -> Self {
            let dir = Dir::default();
            for bytes in objects {
                dir.objects
                    .lock()
                    .unwrap()
                    .insert(object_id(bytes), bytes.to_vec());
            }
            dir
        }
        fn ids(&self) -> BTreeSet<String> {
            self.objects.lock().unwrap().keys().cloned().collect()
        }
    }
    impl ObjectDir for Dir {
        fn list(&self) -> Result<Vec<(String, u64)>> {
            let listed = self
                .objects
                .lock()
                .unwrap()
                .iter()
                .map(|(id, b)| (id.clone(), b.len() as u64))
                .collect();
            for id in self.vanish.lock().unwrap().drain(..) {
                self.objects.lock().unwrap().remove(&id);
            }
            Ok(listed)
        }
        fn read(&self, id: &str, _: u64) -> Result<Vec<u8>> {
            if self.fail_reads {
                bail!("offline");
            }
            self.objects
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .context("gone")
        }
        fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
            if object_id(bytes) != id {
                bail!("identity");
            }
            let mut objects = self.objects.lock().unwrap();
            if let Some(old) = objects.get(id) {
                if old != bytes {
                    bail!("different bytes");
                }
                return Ok(());
            }
            objects.insert(id.into(), bytes.into());
            self.log.lock().unwrap().push(id.into());
            Ok(())
        }
        fn remove(&self, id: &str) -> Result<()> {
            self.objects.lock().unwrap().remove(id);
            Ok(())
        }
    }
    /// Four directories of one replica.
    struct Rep {
        chunks: Dir,
        heads: Dir,
        marks: Dir,
        events: Dir,
    }
    impl Rep {
        fn empty() -> Self {
            Rep {
                chunks: Dir::default(),
                heads: Dir::default(),
                marks: Dir::default(),
                events: Dir::default(),
            }
        }
        fn view(&self) -> ReplicaView<'_, Dir> {
            ReplicaView {
                chunks: &self.chunks,
                heads: &self.heads,
                marks: &self.marks,
                records: vec![&self.events],
            }
        }
    }
    const GATE: &[u8] = b"gate";

    fn old_replicas() -> (Rep, Rep) {
        let a = Rep {
            chunks: Dir::with(&[b"c1"]),
            heads: Dir::with(&[b"h1"]),
            marks: Dir::with(&[]),
            events: Dir::with(&[b"e1", b"e2", GATE]),
        };
        // The second old replica lacks e2 but has e3 (union read).
        let b = Rep {
            chunks: Dir::with(&[b"c1"]),
            heads: Dir::with(&[b"h1"]),
            marks: Dir::with(&[b"m1"]),
            events: Dir::with(&[b"e1", b"e3", GATE]),
        };
        (a, b)
    }

    #[test]
    fn copies_the_union_of_every_kind_with_the_gate_last_and_is_idempotent() {
        let (a, b) = old_replicas();
        let new = Rep::empty();
        let gate = object_id(GATE);
        let copied = backfill(&[a.view(), b.view()], &new.view(), &gate).unwrap();
        assert_eq!(copied, 1 + 1 + 1 + 4);
        assert_eq!(
            new.events.ids(),
            [b"e1".as_slice(), b"e2", b"e3", GATE]
                .iter()
                .map(|b| object_id(b))
                .collect()
        );
        assert_eq!(new.marks.ids(), BTreeSet::from([object_id(b"m1")]));
        assert_eq!(new.events.log.lock().unwrap().last(), Some(&gate));
        // Running again (crash recovery, a second PC) writes nothing.
        assert_eq!(
            backfill(&[a.view(), b.view()], &new.view(), &gate).unwrap(),
            0
        );
    }

    #[test]
    fn unreadable_sources_fail_and_name_the_object() {
        let (mut a, _) = old_replicas();
        a.events.fail_reads = true;
        let new = Rep::empty();
        let error = backfill(&[a.view()], &new.view(), &object_id(GATE)).unwrap_err();
        assert!(
            format!("{error:#}").contains("copying records"),
            "{error:#}"
        );
        // Nothing past the failed directory was written: no gate.
        assert!(!new.events.ids().contains(&object_id(GATE)));
    }

    #[test]
    fn objects_deleted_during_the_copy_are_skipped() {
        let (a, _) = old_replicas();
        a.events.vanish.lock().unwrap().push(object_id(b"e2"));
        let new = Rep::empty();
        backfill(&[a.view()], &new.view(), &object_id(GATE)).unwrap();
        assert!(!new.events.ids().contains(&object_id(b"e2")));
        assert!(new.events.ids().contains(&object_id(b"e1")));
    }

    #[test]
    fn an_object_with_other_bytes_at_its_id_is_refused() {
        let (a, _) = old_replicas();
        let new = Rep::empty();
        let id = object_id(b"e1");
        new.events
            .objects
            .lock()
            .unwrap()
            .insert(id.clone(), b"forged".to_vec());
        // The forged object is listed as present, so it is not overwritten;
        // the copy itself never replaces bytes.
        backfill(&[a.view()], &new.view(), &object_id(GATE)).unwrap();
        assert_eq!(new.events.objects.lock().unwrap()[&id], b"forged".to_vec());
        assert!(new.events.publish(&id, b"e1").is_err());
    }
}
