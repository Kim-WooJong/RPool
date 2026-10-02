//! Append-only namespace journal: one small checksummed delta per save on top
//! of the `namespace.json` checkpoint it is bound to.
//!
//! File layout (`namespace.journal`):
//!
//! ```text
//! header: "RPNSJ001" | checkpoint hash (64 ASCII hex) | blake3(previous 72 bytes)
//! record: payload length (u32 LE) | payload (JSON `Delta`) | chain (32 bytes)
//!         chain = blake3(previous chain | length | payload); the header's
//!         checksum is the first record's previous chain.
//! ```
//!
//! Crash argument. A save appends one record and fsyncs it before it is
//! acknowledged, so only the newest, unacknowledged record can be torn. A
//! writer first truncates the file to the end of the last record it knows
//! to be valid, so a torn record is never followed by another record. At
//! load the records are replayed in order; an invalid record is dropped as a
//! torn tail only when nothing follows it (it reaches past, or exactly to,
//! the end of the file, or the rest is zero fill). Anything else (a damaged
//! header, a damaged record with data after it, a record that does not
//! apply) is corruption and fails the load like a corrupt checkpoint.
//!
//! The header binds the journal to one checkpoint hash. Compaction replaces
//! the checkpoint (whose payload includes every journaled change) before it
//! removes the journal, so a journal that names another checkpoint is stale
//! and ignored: it can only be left over from an interrupted compaction.
use super::{Intent, Namespace};
use crate::mount::shared_model::Event;
use crate::prelude::*;
use std::io::{Read as _, Seek as _, SeekFrom};

/// Journal file of the primary checkpoint `namespace.json`.
pub(super) const JOURNAL: &str = "namespace.journal";
/// Journal of `namespace.previous.json`, copied there by compaction.
pub(super) const PREVIOUS_JOURNAL: &str = "namespace.previous.journal";
/// File signature and format version (first 8 header bytes).
const MAGIC: &[u8; 8] = b"RPNSJ001";
/// Header size in bytes: magic, checkpoint hash (hex), header checksum.
pub(super) const HEADER_LEN: u64 = 8 + 64 + 32;
/// Length prefix plus chain trailer.
pub(super) const RECORD_OVERHEAD: u64 = 4 + 32;
/// A record larger than this is not one this writer produced.
const MAX_RECORD: u64 = 1 << 30;

/// The valid end of a journal as last written or replayed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Tail {
    /// Bytes of header plus valid records; 0 when no journal belongs to the
    /// checkpoint (the next append creates one).
    pub len: u64,
    /// The last 32 bytes before `len`: the chain value to extend.
    pub chain: [u8; 32],
    /// Valid records after the header (bounded by `MAX_JOURNAL_RECORDS`).
    pub records: u64,
}

impl Tail {
    /// Tail of a checkpoint with no journal yet; set after a full checkpoint.
    pub(super) fn empty() -> Self {
        Self {
            len: 0,
            chain: [0; 32],
            records: 0,
        }
    }
}

/// Header bytes binding a journal to `checkpoint` (64 hex chars, else error).
fn header(checkpoint: &str) -> Result<Vec<u8>> {
    if checkpoint.len() != 64 || !checkpoint.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid namespace checkpoint hash");
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN as usize);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(checkpoint.as_bytes());
    let sum = blake3::hash(&bytes);
    bytes.extend_from_slice(sum.as_bytes());
    Ok(bytes)
}

/// Chain value of one record: BLAKE3 of the previous chain, the u32 LE
/// payload length and the payload.
fn chain(previous: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(previous);
    hasher.update(&(payload.len() as u32).to_le_bytes());
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Encodes one record (length, payload, chain) and returns it with its new
/// chain value; empty or oversized payloads are refused.
fn record(previous: &[u8; 32], payload: &[u8]) -> Result<(Vec<u8>, [u8; 32])> {
    if payload.is_empty() || payload.len() as u64 > MAX_RECORD {
        bail!("namespace journal record size out of range");
    }
    let sum = chain(previous, payload);
    let mut bytes = Vec::with_capacity(payload.len() + RECORD_OVERHEAD as usize);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&sum);
    Ok((bytes, sum))
}

/// Appends `payload` after `tail` of the journal bound to `checkpoint`, and
/// flushes it. `Ok(None)`: the journal on disk is not the one `tail`
/// describes (another writer saved since); the caller writes a full
/// checkpoint instead.
pub(super) fn append(
    dir: &Path,
    checkpoint: &str,
    tail: &Tail,
    payload: &[u8],
) -> Result<Option<Tail>> {
    let path = dir.join(JOURNAL);
    let header = header(checkpoint)?;
    if tail.len == 0 {
        // A new journal appears atomically with its first record.
        let seed: [u8; 32] = header[72..].try_into().expect("header checksum");
        let (record, sum) = record(&seed, payload)?;
        let mut bytes = header;
        bytes.extend_from_slice(&record);
        if crate::mount::crash::armed("namespace.journal_torn") {
            let cut = bytes.len() - record.len() / 2;
            super::durable_bytes(&path, &bytes[..cut])?;
            crate::mount::crash::point("namespace.journal_torn")?;
        }
        super::durable_bytes(&path, &bytes)?;
        return Ok(Some(Tail {
            len: bytes.len() as u64,
            chain: sum,
            records: 1,
        }));
    }
    let mut file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    let size = file.metadata()?.len();
    if size < tail.len || tail.len < HEADER_LEN {
        return Ok(None);
    }
    let mut on_disk = vec![0u8; HEADER_LEN as usize];
    file.read_exact(&mut on_disk)?;
    let mut last = [0u8; 32];
    file.seek(SeekFrom::Start(tail.len - 32))?;
    file.read_exact(&mut last)?;
    if on_disk != header || last != tail.chain {
        return Ok(None);
    }
    let (record, sum) = record(&tail.chain, payload)?;
    if size > tail.len {
        // An unacknowledged (possibly torn) record of an earlier attempt.
        file.set_len(tail.len)?;
    }
    file.seek(SeekFrom::Start(tail.len))?;
    if crate::mount::crash::armed("namespace.journal_torn") {
        file.write_all(&record[..record.len() / 2])?;
        file.sync_all()?;
        crate::mount::crash::point("namespace.journal_torn")?;
    }
    file.write_all(&record)?;
    file.sync_all()
        .with_context(|| format!("flush {}", path.display()))?;
    Ok(Some(Tail {
        len: tail.len + record.len() as u64,
        chain: sum,
        records: tail.records + 1,
    }))
}

/// Replays the journal at `path` onto `state`, the checkpoint whose hash is
/// `checkpoint`. A missing or stale journal replays nothing.
pub(super) fn replay(path: &Path, checkpoint: &str, state: &mut Namespace) -> Result<Tail> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Tail::empty()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let header_len = HEADER_LEN as usize;
    if bytes.len() < header_len
        || &bytes[..8] != MAGIC
        || blake3::hash(&bytes[..72]).as_bytes() != &bytes[72..header_len]
    {
        // Journals are created atomically with a complete header.
        bail!("namespace journal header corrupt");
    }
    if &bytes[8..72] != checkpoint.as_bytes() {
        // Left over from a compaction that replaced the checkpoint.
        return Ok(Tail::empty());
    }
    let mut tail = Tail {
        len: HEADER_LEN,
        chain: bytes[72..header_len].try_into().expect("header checksum"),
        records: 0,
    };
    let mut at = header_len;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let torn = || rest.iter().all(|b| *b == 0);
        if rest.len() < 4 {
            break; // torn length prefix
        }
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        if len == 0 {
            if torn() {
                break; // zero fill past the last flushed record
            }
            bail!("namespace journal record {} corrupt", tail.records + 1);
        }
        let end = 4 + len + 32;
        if rest.len() < end {
            break; // torn: the record reaches past the end of the file
        }
        let payload = &rest[4..4 + len];
        let sum = chain(&tail.chain, payload);
        if sum[..] != rest[4 + len..end] {
            if rest.len() == end {
                break; // torn: the last record's bytes were not all flushed
            }
            bail!("namespace journal record {} corrupt", tail.records + 1);
        }
        let delta: Delta = serde_json::from_slice(payload)
            .with_context(|| format!("namespace journal record {} unreadable", tail.records + 1))?;
        delta.apply(state).with_context(|| {
            format!(
                "namespace journal record {} does not apply",
                tail.records + 1
            )
        })?;
        at += end;
        tail = Tail {
            len: at as u64,
            chain: sum,
            records: tail.records + 1,
        };
    }
    Ok(tail)
}

/// How the pending intent list changed between two saves.
#[derive(Debug, Serialize, Deserialize)]
enum PendingDelta {
    /// Drop these intent ids, then append these intents.
    Edit {
        /// Ids of pending intents that are gone (committed or dropped).
        remove: Vec<String>,
        /// Intents added at the end of the list.
        append: Vec<Intent>,
    },
    /// Full replacement list, used when the change is not a removal plus an
    /// append (reordering, edited or duplicate intents).
    Replace(Vec<Intent>),
}

/// The change between two namespaces. Scalars are always recorded; the
/// collections record only added, changed and removed entries.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Delta {
    /// Generation of the new namespace (replay requires consecutive ones).
    generation: u64,
    /// Namespace format version.
    version: u32,
    /// Device id.
    device: String,
    /// Worker name.
    worker: String,
    /// Events added or changed, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    events_put: BTreeMap<String, Event>,
    /// Removed event ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events_del: Vec<String>,
    /// Newly published event ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    published_add: Vec<String>,
    /// Event ids no longer marked published.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    published_del: Vec<String>,
    /// New or changed commit receipts (intent id -> event id).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    committed_put: BTreeMap<String, String>,
    /// Removed commit receipts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    committed_del: Vec<String>,
    /// New or changed edit bases (path -> parent event ids).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bases_put: BTreeMap<String, Vec<String>>,
    /// Paths whose base was removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bases_del: Vec<String>,
    /// Directories created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    directories_add: Vec<String>,
    /// Directories removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    directories_del: Vec<String>,
    /// Pending-list change; `None` when the list is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingDelta>,
}

/// Entries of `new` that are missing from or differ in `old`, and keys of
/// `old` missing from `new`, in one merge walk.
fn diff_map<V: Clone>(
    old: &BTreeMap<String, V>,
    new: &BTreeMap<String, V>,
    same: impl Fn(&V, &V) -> bool,
) -> (BTreeMap<String, V>, Vec<String>) {
    let (mut put, mut del) = (BTreeMap::new(), vec![]);
    let (mut a, mut b) = (old.iter().peekable(), new.iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some((k, _)), None) => {
                del.push((*k).clone());
                a.next();
            }
            (None, Some((k, v))) => {
                put.insert((*k).clone(), (*v).clone());
                b.next();
            }
            (Some((ka, va)), Some((kb, vb))) => match ka.cmp(kb) {
                std::cmp::Ordering::Less => {
                    del.push((*ka).clone());
                    a.next();
                }
                std::cmp::Ordering::Greater => {
                    put.insert((*kb).clone(), (*vb).clone());
                    b.next();
                }
                std::cmp::Ordering::Equal => {
                    if !same(va, vb) {
                        put.insert((*kb).clone(), (*vb).clone());
                    }
                    a.next();
                    b.next();
                }
            },
        }
    }
    (put, del)
}

/// Added and removed members of a set (`new - old`, `old - new`).
fn diff_set(old: &BTreeSet<String>, new: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    (
        new.difference(old).cloned().collect(),
        old.difference(new).cloned().collect(),
    )
}

/// Minimal `PendingDelta` from `old` to `new`; `None` when equal.
fn diff_pending(old: &[Intent], new: &[Intent]) -> Option<PendingDelta> {
    if old == new {
        return None;
    }
    let ids: BTreeSet<&str> = new.iter().map(|i| i.id.as_str()).collect();
    let (kept, removed): (Vec<&Intent>, Vec<&Intent>) =
        old.iter().partition(|i| ids.contains(i.id.as_str()));
    let unique = old.iter().map(|i| &i.id).collect::<BTreeSet<_>>().len() == old.len();
    if unique && kept.len() <= new.len() && kept.iter().zip(new).all(|(a, b)| *a == b) {
        Some(PendingDelta::Edit {
            remove: removed.into_iter().map(|i| i.id.clone()).collect(),
            append: new[kept.len()..].to_vec(),
        })
    } else {
        Some(PendingDelta::Replace(new.to_vec()))
    }
}

impl Delta {
    /// What turns `old` into `new`. Collections that are still the same
    /// shared value are skipped without a walk. An event id present in both
    /// is the same event: any write to the event map invalidates the cached
    /// projection, and the projection `save` validates first rejects an
    /// event that does not hash to its id.
    pub(super) fn between(old: &Namespace, new: &Namespace) -> Self {
        use super::shared::Shared;
        let mut delta = Self {
            generation: new.generation,
            version: new.version,
            device: new.device.clone(),
            worker: new.worker.clone(),
            ..Self::default()
        };
        if !Shared::ptr_eq(&old.events, &new.events) {
            (delta.events_put, delta.events_del) = diff_map(&old.events, &new.events, |_, _| true);
        }
        if !Shared::ptr_eq(&old.published, &new.published) {
            (delta.published_add, delta.published_del) = diff_set(&old.published, &new.published);
        }
        if !Shared::ptr_eq(&old.committed_intents, &new.committed_intents) {
            (delta.committed_put, delta.committed_del) =
                diff_map(&old.committed_intents, &new.committed_intents, |a, b| {
                    a == b
                });
        }
        if !Shared::ptr_eq(&old.bases, &new.bases) {
            (delta.bases_put, delta.bases_del) = diff_map(&old.bases, &new.bases, |a, b| a == b);
        }
        if !Shared::ptr_eq(&old.directories, &new.directories) {
            (delta.directories_add, delta.directories_del) =
                diff_set(&old.directories, &new.directories);
        }
        delta.pending = diff_pending(&old.pending, &new.pending);
        delta
    }

    /// Whether this records nothing but the next generation of `old`.
    pub(super) fn is_noop(&self, old: &Namespace) -> bool {
        self.version == old.version
            && self.device == old.device
            && self.worker == old.worker
            && self.events_put.is_empty()
            && self.events_del.is_empty()
            && self.published_add.is_empty()
            && self.published_del.is_empty()
            && self.committed_put.is_empty()
            && self.committed_del.is_empty()
            && self.bases_put.is_empty()
            && self.bases_del.is_empty()
            && self.directories_add.is_empty()
            && self.directories_del.is_empty()
            && self.pending.is_none()
    }

    /// Applies this record to the state it was computed from. Removing an
    /// absent entry or skipping a generation means the record does not
    /// belong here.
    pub(super) fn apply(self, state: &mut Namespace) -> Result<()> {
        if Some(self.generation) != state.generation.checked_add(1) {
            bail!("namespace journal generation gap");
        }
        state.generation = self.generation;
        state.version = self.version;
        state.device = self.device;
        state.worker = self.worker;
        fn remove_all<V>(map: &mut BTreeMap<String, V>, keys: Vec<String>) -> Result<()> {
            for key in keys {
                map.remove(&key)
                    .context("journal removes an absent entry")?;
            }
            Ok(())
        }
        fn set_edit(set: &mut BTreeSet<String>, add: Vec<String>, del: Vec<String>) -> Result<()> {
            for key in del {
                if !set.remove(&key) {
                    bail!("journal removes an absent entry");
                }
            }
            set.extend(add);
            Ok(())
        }
        if !self.events_del.is_empty() || !self.events_put.is_empty() {
            let events = &mut *state.events;
            remove_all(events, self.events_del)?;
            events.extend(self.events_put);
        }
        if !self.published_add.is_empty() || !self.published_del.is_empty() {
            set_edit(&mut state.published, self.published_add, self.published_del)?;
        }
        if !self.committed_del.is_empty() || !self.committed_put.is_empty() {
            let committed = &mut *state.committed_intents;
            remove_all(committed, self.committed_del)?;
            committed.extend(self.committed_put);
        }
        if !self.bases_del.is_empty() || !self.bases_put.is_empty() {
            let bases = &mut *state.bases;
            remove_all(bases, self.bases_del)?;
            bases.extend(self.bases_put);
        }
        if !self.directories_add.is_empty() || !self.directories_del.is_empty() {
            set_edit(
                &mut state.directories,
                self.directories_add,
                self.directories_del,
            )?;
        }
        match self.pending {
            None => {}
            Some(PendingDelta::Replace(pending)) => state.pending = pending,
            Some(PendingDelta::Edit { remove, append }) => {
                let remove: BTreeSet<String> = remove.into_iter().collect();
                let before = state.pending.len();
                state.pending.retain(|i| !remove.contains(&i.id));
                if before - state.pending.len() != remove.len() {
                    bail!("journal removes an absent pending intent");
                }
                state.pending.extend(append);
            }
        }
        Ok(())
    }
}
