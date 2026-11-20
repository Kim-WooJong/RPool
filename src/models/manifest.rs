//! Archive manifest ([`Manifest`]) and its shard entries (`Shard`): the
//! JSON document stored next to the shards that describes how to rebuild a
//! file. Loaded and validated by the `manifest` module.
use super::{Coding, ShardKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "ManifestWire")]
/// Everything needed to restore one archive: original file, coding, and the
/// location and hash of every shard.
pub(crate) struct Manifest {
    /// Manifest format version: legacy 1/2, immutable ownership-aware 3.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) shared: Option<SharedObjects>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ObjectIdentity {
    pub index: u32, pub owner: String, pub remote: String, pub object: String,
    pub size: u64, pub blake3: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SharedObjects {
    pub capability: String,
    pub source_fingerprint: String,
    pub objects: Vec<ObjectIdentity>,
}

#[derive(Deserialize)]
struct ManifestWire {
    version: u32, archive_id: String, original_name: String, original_size: u64,
    shard_size: u64, created_unix: u64, content_root_blake3: String,
    #[serde(default)] coding: Option<Coding>, shards: Vec<Shard>,
    #[serde(default)] shared: Option<SharedObjects>,
}
impl TryFrom<ManifestWire> for Manifest {
    type Error = String;
    fn try_from(w: ManifestWire) -> Result<Self, Self::Error> {
        let m = Self { version:w.version, archive_id:w.archive_id, original_name:w.original_name,
            original_size:w.original_size, shard_size:w.shard_size, created_unix:w.created_unix,
            content_root_blake3:w.content_root_blake3, coding:w.coding, shards:w.shards, shared:w.shared };
        // Covers direct serde callers as well as the common loader. Legacy
        // parse-only behavior is preserved; v3 can never bypass ownership validation.
        if m.version == 3 { crate::manifest::validate_manifest(&m).map_err(|e|e.to_string())?; }
        else if m.shared.is_some() || !matches!(m.version, 1 | 2) { return Err("unsupported manifest capability/version".into()); }
        Ok(m)
    }
}
