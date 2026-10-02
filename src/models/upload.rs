//! Upload planning types: the `UploadPlan` built by `planning::upload_plan`
//! before shards are written, the size-only [`PhysicalSpec`] used for quota
//! checks, and [`GeneratedParity`] files produced by `erasure::encode`.
use super::{Coding, Placement, ShardKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Where every shard of a new archive goes; its fingerprint ties the upload
/// journal to it.
pub(crate) struct UploadPlan {
    /// Plan format version (2).
    pub(crate) version: u32,
    /// Id of the archive being created.
    pub(crate) archive_id: String,
    /// Size of the file being uploaded, bytes.
    pub(crate) source_size: u64,
    /// Bytes per data shard.
    pub(crate) shard_size: u64,
    /// Target remotes.
    pub(crate) remotes: Vec<String>,
    /// Placement policy that assigned the remotes.
    pub(crate) placement: Placement,
    #[serde(default)]
    /// Erasure coding; `None` for uncoded uploads.
    pub(crate) coding: Option<Coding>,
    /// Every planned data and parity shard.
    pub(crate) shards: Vec<PlanShard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One planned shard (becomes a manifest [`Shard`](super::Shard) once stored).
pub(crate) struct PlanShard {
    /// Shard number.
    pub(crate) index: u32,
    /// Offset in the source file (0 for parity).
    pub(crate) offset: u64,
    /// Planned size, bytes.
    pub(crate) size: u64,
    /// Destination remote.
    pub(crate) remote: String,
    /// Destination object address.
    pub(crate) object: String,
    #[serde(default)]
    /// Data or parity.
    pub(crate) kind: ShardKind,
    #[serde(default)]
    /// Reed-Solomon group.
    pub(crate) group: u32,
    #[serde(default)]
    /// Position in the group (data first, then parity).
    pub(crate) slot: u16,
}

#[derive(Debug, Clone)]
/// Group and size of one shard to be stored, without a destination; built by
/// `planning::upload_plan` for capacity checks (e.g. migration quota checks).
pub(crate) struct PhysicalSpec {
    /// Reed-Solomon group.
    pub(crate) group: u32,
    /// Stored size, bytes.
    pub(crate) size: u64,
}

#[derive(Debug)]
/// A parity shard written to a local temporary file, ready for upload.
pub(crate) struct GeneratedParity {
    /// Its planned shard.
    pub(crate) plan: PlanShard,
    /// Local temporary file holding the parity bytes.
    pub(crate) path: PathBuf,
    /// BLAKE3 hex of the parity bytes.
    pub(crate) blake3: String,
}
