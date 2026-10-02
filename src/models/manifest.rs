//! Archive manifest ([`Manifest`]) and its shard entries (`Shard`): the
//! JSON document stored next to the shards that describes how to rebuild a
//! file. Loaded and validated by the `manifest` module.
use super::{Coding, ShardKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Everything needed to restore one archive: original file, coding, and the
/// location and hash of every shard.
pub(crate) struct Manifest {
    /// Manifest format version (1 or 2; relocation requires 2).
    pub(crate) version: u32,
    /// Unique archive id; also the archive's folder name on each remote.
    pub(crate) archive_id: String,
    /// File name of the original file.
    pub(crate) original_name: String,
    /// Original file size in bytes.
    pub(crate) original_size: u64,
    /// Bytes per data shard (the last data shard may be shorter).
    pub(crate) shard_size: u64,
    /// Creation time, Unix seconds.
    pub(crate) created_unix: u64,
    /// BLAKE3 content root over the shard list (`manifest::content_root`).
    pub(crate) content_root_blake3: String,
    #[serde(default)]
    /// Erasure coding; `None` for uncoded archives.
    pub(crate) coding: Option<Coding>,
    /// Every data and parity shard.
    pub(crate) shards: Vec<Shard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One stored shard of an archive.
pub(crate) struct Shard {
    /// Shard number, unique within the manifest.
    pub(crate) index: u32,
    /// Byte offset of a data shard in the original file (0 for parity).
    pub(crate) offset: u64,
    /// Stored size in bytes.
    pub(crate) size: u64,
    /// Remote (`name:path`) the shard lives on.
    pub(crate) remote: String,
    /// Full object address of the shard.
    pub(crate) object: String,
    /// BLAKE3 hex of the shard bytes.
    pub(crate) blake3: String,
    #[serde(default)]
    /// Data or parity (defaults to data for v1 manifests).
    pub(crate) kind: ShardKind,
    #[serde(default)]
    /// Reed-Solomon group number.
    pub(crate) group: u32,
    #[serde(default)]
    /// Position within the group: data slots first, then parity.
    pub(crate) slot: u16,
}
