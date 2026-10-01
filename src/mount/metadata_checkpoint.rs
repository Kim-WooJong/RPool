//! Reading checkpoints from every replica: listings, verified heads and
//! chunks, and the checkpoint part of a pull. Remote errors while *listing*
//! propagate (an outage is never an empty namespace); an unreadable or invalid
//! head/chunk only makes that head unusable.
use super::metadata_cache::Cache;
use super::metadata_checkpoint_model::{Chunk, Family, Head, Ids};
use super::metadata_dir::ObjectDir;
use crate::prelude::*;

/// One replica's directories of a family root.
pub(crate) struct Replica<'a> {
    /// Record directories, aligned with `Family::kinds`.
    pub records: Vec<&'a dyn ObjectDir>,
    pub heads: &'a dyn ObjectDir,
    pub chunks: &'a dyn ObjectDir,
    pub marks: &'a dyn ObjectDir,
}

/// id -> (size, replica indices listing it).
pub(crate) type Listing = BTreeMap<String, (u64, BTreeSet<usize>)>;

pub(crate) fn list_all(dirs: &[&dyn ObjectDir]) -> Result<Listing> {
    let mut result = Listing::new();
    for (index, dir) in dirs.iter().enumerate() {
        for (id, size) in dir.list()? {
            let entry = result.entry(id).or_insert((size, BTreeSet::new()));
            if entry.0 != size {
                bail!("metadata replica size mismatch; retain workspace");
            }
            entry.1.insert(index);
        }
    }
    Ok(result)
}

/// Reads `id` from the first replica that lists it and verifies it.
pub(crate) fn read_any(
    dirs: &[&dyn ObjectDir],
    listing: &Listing,
    id: &str,
    mut accept: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let (size, holders) = listing.get(id).context("metadata object not listed")?;
    let mut last = None;
    for index in holders {
        match dirs[*index]
            .read(id, *size)
            .and_then(|bytes| accept(&bytes))
        {
            Ok(()) => return Ok(()),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("metadata object unavailable")))
}

/// Valid heads and the record ids of every chunk they use.
#[derive(Default)]
pub(crate) struct Survey {
    pub heads: BTreeMap<String, Head>,
    pub head_listing: Listing,
    pub chunk_listing: Listing,
    /// Heads that are listed but unusable (invalid, unreadable, missing chunk).
    pub broken: usize,
}
impl Survey {
    /// Chunks of every usable head (the union a new head must keep).
    pub(crate) fn chunks(&self) -> BTreeSet<String> {
        self.heads.values().flat_map(|h| h.chunks.clone()).collect()
    }
    /// The head whose chunks include every other usable head's chunks.
    pub(crate) fn newest(&self) -> Option<(&String, &Head)> {
        let all = self.chunks();
        self.heads
            .iter()
            .filter(|(_, h)| h.chunks.len() == all.len())
            .max_by_key(|(_, h)| h.created_unix)
    }
    pub(crate) fn newest_unix(&self) -> Option<u64> {
        self.heads.values().map(|h| h.created_unix).max()
    }
    /// Head and every chunk listed on each of `replicas` replicas.
    pub(crate) fn everywhere(&self, id: &str, replicas: usize) -> bool {
        let full = |listing: &Listing, id: &str| {
            listing
                .get(id)
                .is_some_and(|(_, holders)| holders.len() == replicas)
        };
        full(&self.head_listing, id)
            && self.heads.get(id).is_some_and(|h| {
                h.chunks
                    .iter()
                    .all(|chunk| full(&self.chunk_listing, chunk))
            })
    }
}

/// Lists and validates heads; reads chunks not in `cache` (passing each new
/// record to `sink` as `(kind, id, exact JSON)`) and records their ids.
pub(crate) fn survey(
    family: &Family,
    replicas: &[Replica<'_>],
    cache: &mut Cache,
    sink: &mut dyn FnMut(&str, &str, &str) -> Result<()>,
) -> Result<Survey> {
    let heads: Vec<_> = replicas.iter().map(|r| r.heads).collect();
    let chunks: Vec<_> = replicas.iter().map(|r| r.chunks).collect();
    let mut result = Survey {
        head_listing: list_all(&heads)?,
        ..Default::default()
    };
    if result.head_listing.is_empty() {
        return Ok(result);
    }
    result.chunk_listing = list_all(&chunks)?;
    for id in result.head_listing.keys().cloned().collect::<Vec<_>>() {
        let mut head = None;
        let read = read_any(&heads, &result.head_listing, &id, |bytes| {
            head = Some(Head::parse(&id, bytes, family)?);
            Ok(())
        });
        let Some(head) = head.filter(|_| read.is_ok()) else {
            eprintln!(
                "Checkpoint head {} ignored: {:#}",
                &id[..12],
                read.unwrap_err()
            );
            result.broken += 1;
            continue;
        };
        let mut usable = true;
        for chunk_id in &head.chunks {
            if cache.chunks.contains_key(chunk_id) {
                continue;
            }
            let mut ids: Option<Ids> = None;
            let read = read_any(&chunks, &result.chunk_listing, chunk_id, |bytes| {
                let chunk = Chunk::parse(chunk_id, bytes, family)?;
                for (kind, records) in &chunk.records {
                    for (record, text) in records {
                        sink(kind, record, text)?;
                    }
                }
                ids = Some(chunk.ids());
                Ok(())
            });
            match (read, ids) {
                (Ok(()), Some(ids)) => {
                    cache.chunks.insert(chunk_id.clone(), ids);
                }
                (result, _) => {
                    eprintln!(
                        "Checkpoint chunk {} unusable: {:#}",
                        &chunk_id[..12],
                        result.err().unwrap_or_else(|| anyhow!("no records"))
                    );
                    usable = false;
                    break;
                }
            }
        }
        if usable {
            result.heads.insert(id, head);
        } else {
            result.broken += 1;
        }
    }
    Ok(result)
}

/// Record ids covered by the chunks of `head` (all in `cache`).
pub(crate) fn covered_by(head: &Head, cache: &Cache) -> Ids {
    let mut ids = Ids::new();
    for chunk in &head.chunks {
        for (kind, set) in cache.chunks.get(chunk).into_iter().flatten() {
            ids.entry(kind.clone())
                .or_default()
                .extend(set.iter().cloned());
        }
    }
    ids
}

/// The checkpoint part of a pull: needed on a bootstrap (nothing known yet)
/// or once the gate exists (covered records may be deleted). New records go
/// to `sink`; the returned ids are every record any usable head covers.
pub(crate) fn pull(
    family: &Family,
    replicas: &[Replica<'_>],
    cache: &mut Cache,
    gate: bool,
    bootstrap: bool,
    sink: &mut dyn FnMut(&str, &str, &str) -> Result<()>,
) -> Result<Ids> {
    cache.gate |= gate;
    if !(bootstrap || cache.gate) {
        return Ok(Ids::new());
    }
    if bootstrap {
        // A workspace without records must not trust chunks it once read.
        cache.chunks.clear();
    }
    let survey = survey(family, replicas, cache, sink)?;
    if cache.gate && survey.broken > 0 {
        bail!("a pool metadata checkpoint is unreadable while deletion of covered records is enabled; retain this workspace and retry (run `rpool doctor`)");
    }
    let mut covered = Ids::new();
    for head in survey.heads.values() {
        for (kind, ids) in covered_by(head, cache) {
            covered.entry(kind).or_default().extend(ids);
        }
    }
    Ok(covered)
}
