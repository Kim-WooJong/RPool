//! Pure checkpoint objects: lossless chunks of exact record bytes, incremental
//! heads, deletion marks, and the per-family compaction gate record.
use super::metadata_dir::{object_id, valid_id};
use super::metadata_limits::RECORD_BYTES_MAX;
use crate::prelude::*;

pub(crate) const FORMAT: u32 = 1;
/// Records per chunk stay below this many JSON bytes (chunk limit is 8 MiB).
pub(crate) const CHUNK_TARGET: usize = 6 * 1024 * 1024;
/// Marked ids per mark object (keeps a mark far below 8 MiB).
pub(crate) const MARK_IDS_MAX: usize = 100_000;

/// A record family sharing one metadata root.
#[derive(Debug, Clone)]
pub(crate) struct Family {
    pub name: &'static str,
    /// Record directories in replica order (`events`, or `snapshots`, `names`).
    pub kinds: &'static [&'static str],
    /// Exact bytes of the gate record, published in `kinds[0]`.
    pub gate: Vec<u8>,
}
impl Family {
    pub(crate) fn gate_id(&self) -> String {
        object_id(&self.gate)
    }
    /// v6: an event older RPool rejects ("unsupported namespace event version").
    pub(crate) fn v6() -> Self {
        let gate = super::shared_model::Event {
            version: 2,
            worker: "rpool-compaction".into(),
            device: format!("format-{FORMAT}"),
            path: "rpool-compaction-gate".into(),
            parents: vec![],
            content: None,
        };
        Self {
            name: "v6",
            kinds: &["events"],
            gate: serde_json::to_vec(&gate).expect("gate serializes"),
        }
    }
    /// v7: a snapshot older RPool rejects (version is not 7).
    pub(crate) fn v7() -> Self {
        let zero = "0".repeat(64);
        let gate = super::peer_snapshot_model::Snapshot {
            version: 8,
            file_id: zero.clone(),
            owner_id: object_id(format!("rpool-compaction-gate-{FORMAT}").as_bytes()),
            policy: super::peer_snapshot_model::Policy {
                genesis_id: zero,
                history_limit: 0,
            },
            parents: BTreeSet::new(),
            covered: BTreeSet::new(),
            revisions: BTreeMap::new(),
            heads: BTreeSet::new(),
            payloads: BTreeMap::new(),
            unavailable: BTreeSet::new(),
        };
        Self {
            name: "v7",
            kinds: &["snapshots", "names"],
            gate: serde_json::to_vec(&gate).expect("gate serializes"),
        }
    }
    fn has_kind(&self, kind: &str) -> bool {
        self.kinds.contains(&kind)
    }
}

pub(crate) type Records = BTreeMap<String, BTreeMap<String, String>>;
pub(crate) type Ids = BTreeMap<String, BTreeSet<String>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Chunk {
    pub format: u32,
    pub family: String,
    /// kind -> record id -> exact record JSON.
    pub records: Records,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Head {
    pub format: u32,
    pub family: String,
    pub created_unix: u64,
    /// Sorted, unique chunk ids; a superset of every head it replaced.
    pub chunks: Vec<String>,
    pub records: u64,
    pub bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mark {
    pub format: u32,
    pub family: String,
    pub checkpoint: String,
    pub marked_unix: u64,
    pub records: Ids,
}

fn header(format: u32, family: &str, expected: &Family) -> Result<()> {
    if format != FORMAT || family != expected.name {
        bail!("unsupported checkpoint format or family");
    }
    Ok(())
}
impl Chunk {
    pub(crate) fn parse(id: &str, bytes: &[u8], family: &Family) -> Result<Self> {
        if object_id(bytes) != id || bytes.len() > RECORD_BYTES_MAX {
            bail!("checkpoint chunk identity mismatch");
        }
        let chunk: Self = serde_json::from_slice(bytes)?;
        header(chunk.format, &chunk.family, family)?;
        if chunk.records.is_empty() {
            bail!("empty checkpoint chunk");
        }
        for (kind, records) in &chunk.records {
            if !family.has_kind(kind) || records.is_empty() {
                bail!("invalid checkpoint chunk kind");
            }
            for (record, text) in records {
                if !valid_id(record) || object_id(text.as_bytes()) != *record {
                    bail!("checkpoint record identity mismatch");
                }
                if *record == family.gate_id() {
                    bail!("checkpoint contains the gate record");
                }
            }
        }
        Ok(chunk)
    }
    pub(crate) fn ids(&self) -> Ids {
        self.records
            .iter()
            .map(|(kind, r)| (kind.clone(), r.keys().cloned().collect()))
            .collect()
    }
}
impl Head {
    pub(crate) fn parse(id: &str, bytes: &[u8], family: &Family) -> Result<Self> {
        if object_id(bytes) != id {
            bail!("checkpoint head identity mismatch");
        }
        let head: Self = serde_json::from_slice(bytes)?;
        header(head.format, &head.family, family)?;
        if head.chunks.is_empty()
            || head.chunks.iter().any(|c| !valid_id(c))
            || head.chunks.windows(2).any(|w| w[0] >= w[1])
        {
            bail!("invalid checkpoint head chunk list");
        }
        Ok(head)
    }
}
impl Mark {
    pub(crate) fn parse(id: &str, bytes: &[u8], family: &Family) -> Result<Self> {
        if object_id(bytes) != id {
            bail!("checkpoint mark identity mismatch");
        }
        let mark: Self = serde_json::from_slice(bytes)?;
        header(mark.format, &mark.family, family)?;
        if !valid_id(&mark.checkpoint)
            || mark
                .records
                .iter()
                .any(|(k, ids)| !family.has_kind(k) || ids.iter().any(|i| !valid_id(i)))
        {
            bail!("invalid checkpoint mark");
        }
        Ok(mark)
    }
}

/// Streams records into chunks below [`CHUNK_TARGET`], so at most one chunk
/// is held in memory. Records too large for any chunk are refused by `push`
/// (they stay individual objects and are never deleted).
pub(crate) struct Packer<'a> {
    family: &'a Family,
    current: Records,
    size: usize,
}
impl<'a> Packer<'a> {
    pub(crate) fn new(family: &'a Family) -> Self {
        Self {
            family,
            current: Records::new(),
            size: 0,
        }
    }
    /// `Ok(false)`: the record does not fit any chunk. A full chunk is
    /// returned through `ready` before the record is added.
    pub(crate) fn push(
        &mut self,
        kind: &str,
        id: &str,
        text: String,
        ready: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<bool> {
        // Escaped JSON string plus the key and punctuation.
        let cost = serde_json::to_string(&text)?.len() + id.len() + kind.len() + 8;
        if cost > CHUNK_TARGET {
            return Ok(false);
        }
        if self.size + cost > CHUNK_TARGET {
            ready.extend(self.flush()?);
        }
        self.current
            .entry(kind.into())
            .or_default()
            .insert(id.into(), text);
        self.size += cost;
        Ok(true)
    }
    pub(crate) fn flush(&mut self) -> Result<Option<(String, Vec<u8>)>> {
        if self.current.is_empty() {
            return Ok(None);
        }
        self.size = 0;
        let chunk = Chunk {
            format: FORMAT,
            family: self.family.name.into(),
            records: std::mem::take(&mut self.current),
        };
        let bytes = serde_json::to_vec(&chunk)?;
        if bytes.len() > RECORD_BYTES_MAX {
            bail!("checkpoint chunk exceeds the object limit");
        }
        Ok(Some((object_id(&bytes), bytes)))
    }
}

pub(crate) fn head_bytes(head: &Head) -> Result<(String, Vec<u8>)> {
    let bytes = serde_json::to_vec(head)?;
    Ok((object_id(&bytes), bytes))
}
pub(crate) fn mark_bytes(mark: &Mark) -> Result<(String, Vec<u8>)> {
    let bytes = serde_json::to_vec(mark)?;
    if bytes.len() > RECORD_BYTES_MAX {
        bail!("checkpoint mark exceeds the object limit");
    }
    Ok((object_id(&bytes), bytes))
}
