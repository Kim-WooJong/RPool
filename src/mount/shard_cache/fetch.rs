//! The job behind one flight: a direct, streamed download of the shard into
//! a partial file that becomes the cache entry only after BLAKE3
//! verification, with Reed-Solomon reconstruction of its group as the
//! fallback for demand reads.
use super::flight::{Flight, Outcome, ProgressSink};
use super::index::Admission;
use super::{Inner, PARTIAL_PREFIX};
use crate::prelude::*;
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::storage::reader::{is_restore_unavailable, StorageReader};

pub(super) struct Job {
    pub(super) shard: Shard,
    /// Recovery group of the shard; `None` for plain archives and readahead.
    pub(super) group: Option<Manifest>,
    pub(super) workers: usize,
    pub(super) retries: u32,
}

impl Job {
    pub(super) fn new(
        m: &Manifest,
        s: &Shard,
        mode: Admission,
        workers: usize,
        retries: u32,
    ) -> Result<Self> {
        // Readahead never reconstructs: a restore reserves a whole group's
        // working set, which only a reader that needs the bytes may claim.
        let group = match (mode, m.coding.is_some()) {
            (Admission::Demand, true) => Some(group_manifest(m, s.group)?),
            _ => None,
        };
        Ok(Self {
            shard: s.clone(),
            group,
            workers,
            retries,
        })
    }
}

impl Inner {
    /// Run a flight to its end: retire it from the flight table, then publish
    /// its outcome to waiters. Returns the original error to an inline leader.
    /// A panic still retires the flight (as failed), so waiters never hang.
    pub(super) fn run(
        &self,
        reader: &StorageReader,
        job: &Job,
        flight: &Arc<Flight>,
    ) -> Result<()> {
        let mut retire = Retire {
            inner: self,
            shard: &job.shard,
            flight,
            outcome: None,
        };
        let result = self.fetch(reader, job, flight);
        retire.outcome = Some(match &result {
            Ok(()) => Outcome::Published,
            Err(error) => Outcome::Failed {
                message: format!("{error:#}"),
                retry: flight.mode == Admission::Prefetch,
            },
        });
        drop(retire);
        result
    }
    /// Remove a flight from the table, then wake its waiters with `outcome`.
    /// Retired before finishing, so a woken reader never rejoins this flight.
    pub(super) fn retire(&self, shard: &Shard, flight: &Arc<Flight>, outcome: Outcome) {
        {
            let mut st = self.state();
            let name = super::entry_name(shard);
            if st
                .flights
                .get(&name)
                .is_some_and(|f| Arc::ptr_eq(f, flight))
            {
                st.flights.remove(&name);
            }
            if flight.mode == Admission::Prefetch {
                st.prefetching = st.prefetching.saturating_sub(1);
            }
        }
        flight.finish(outcome);
        self.space.notify_all();
    }

    fn fetch(&self, reader: &StorageReader, job: &Job, flight: &Flight) -> Result<()> {
        let s = &job.shard;
        if self.valid(s)? {
            // Already on disk (published by a group restore): adopt it.
            self.admit_entry(s)?;
            return Ok(());
        }
        let reservation = self.reserve(s.size, flight.mode)?;
        let mut temp = tempfile::Builder::new()
            .prefix(PARTIAL_PREFIX)
            .tempfile_in(&self.root)?;
        flight.stream_to(File::open(temp.path())?);
        let direct = reader.verified_read(
            s,
            &mut ProgressSink {
                file: temp.as_file_mut(),
                flight,
            },
        );
        match direct {
            Ok(()) => {
                temp.as_file().sync_all()?;
                temp.persist(self.path(s)).map_err(|e| e.error)?;
                self.remember_verified(s)?;
                self.admit_entry(s)?;
                drop(reservation);
                Ok(())
            }
            Err(error) => {
                if flight.stop_stream() && is_corrupt(&error) {
                    // Integrity trade-off of prefix serving (see `ShardCache`):
                    // bytes served before verification came from this stream.
                    eprintln!(
                        "warning: shard {} failed verification after part of it was served unverified; later reads reconstruct it",
                        s.object
                    );
                }
                // Drop the failed direct-read bytes before reserving recovery space.
                drop(temp);
                drop(reservation);
                match &job.group {
                    Some(mini)
                        if flight.mode == Admission::Demand && is_restore_unavailable(&error) =>
                    {
                        self.recover(reader, mini, job)
                    }
                    _ => Err(error),
                }
            }
        }
    }

    fn recover(&self, reader: &StorageReader, mini: &Manifest, job: &Job) -> Result<()> {
        let manifest_bytes = serde_json::to_vec_pretty(mini)?.len() as u64;
        // Restore holds the group output, staged shard attempts (including
        // failed attempts), and published data concurrently. Include bounded
        // resume JSON and its atomic replacement, even outside this root.
        let staged = mini.shards.iter().try_fold(0u64, |n, shard| {
            n.checked_add(
                shard
                    .size
                    .checked_mul(job.retries.max(1) as u64)
                    .context("recovery cache size overflow")?,
            )
            .context("recovery cache size overflow")
        })?;
        let required = mini
            .original_size
            .checked_mul(2)
            .and_then(|n| n.checked_add(staged))
            .and_then(|n| n.checked_add(manifest_bytes))
            .and_then(|n| n.checked_add(8192 + mini.shards.len() as u64 * 32))
            .context("recovery cache size overflow")?;
        let _reservation = self.reserve(required, Admission::Demand)?;
        let stage = tempfile::tempdir_in(&self.root)?;
        let manifest = stage.path().join("manifest.json");
        crate::mount::namespace::durable_json(&manifest, mini)?;
        let output = stage.path().join("group");
        crate::commands::get_with_storage(
            reader,
            &manifest.to_string_lossy(),
            &output,
            job.workers,
            job.retries,
        )?;
        for shard in crate::manifest::data_shards(mini) {
            self.publish(shard, &output, shard.offset)?;
        }
        Ok(())
    }

    /// Copy verified restored bytes into the cache. A group-relative shard
    /// has the same content name as the original shard.
    fn publish(&self, s: &Shard, source: &Path, offset: u64) -> Result<()> {
        if self.state().index.contains(&super::entry_name(s)) {
            return Ok(());
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        let mut input = File::open(source)?;
        input.seek(SeekFrom::Start(offset))?;
        let n = std::io::copy(&mut input.take(s.size), &mut temp)?;
        if n != s.size || crate::utils::hash_file_range(temp.path(), 0, s.size)? != s.blake3 {
            bail!("invalid recovered cache bytes");
        }
        temp.as_file().sync_all()?;
        temp.persist(self.path(s)).map_err(|e| e.error)?;
        self.remember_verified(s)?;
        self.admit_entry(s)
    }
}

struct Retire<'a> {
    inner: &'a Inner,
    shard: &'a Shard,
    flight: &'a Arc<Flight>,
    outcome: Option<Outcome>,
}
impl Drop for Retire<'_> {
    fn drop(&mut self) {
        let outcome = self.outcome.take().unwrap_or_else(|| Outcome::Failed {
            message: "shard download aborted".into(),
            retry: false,
        });
        self.inner.retire(self.shard, self.flight, outcome);
    }
}

fn is_corrupt(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::CorruptData)
}

pub(super) fn group_manifest(m: &Manifest, group: u32) -> Result<Manifest> {
    let coding = m
        .coding
        .clone()
        .context("plain archive has no recovery group")?;
    let mut shards = vec![];
    let mut offset = 0u64;
    for old in crate::manifest::data_shards(m)
        .into_iter()
        .filter(|s| s.group == group)
    {
        let mut s = old.clone();
        s.index = shards.len() as u32;
        s.offset = offset;
        s.group = 0;
        offset += s.size;
        shards.push(s);
    }
    let data_count = shards.len();
    for old in m
        .shards
        .iter()
        .filter(|s| s.group == group && s.kind == ShardKind::Parity)
    {
        let mut s = old.clone();
        s.index = (data_count + s.slot as usize - coding.data_shards) as u32;
        s.group = 0;
        shards.push(s);
    }
    let root =
        crate::manifest::content_root_v2(offset, m.shard_size, &Some(coding.clone()), &shards);
    let mini = Manifest {
        version: 2,
        archive_id: format!("{}-group-{group}", m.archive_id),
        original_name: m.original_name.clone(),
        original_size: offset,
        shard_size: m.shard_size,
        created_unix: m.created_unix,
        content_root_blake3: root,
        coding: Some(coding),
        shards,
    };
    crate::manifest::validate_manifest(&mini)?;
    Ok(mini)
}
