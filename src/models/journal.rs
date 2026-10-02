//! Resume journal of one upload ([`UploadJournal`]), managed by
//! `journal::upload`.
use super::Shard;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Shards an upload already stored, valid only for the plan it was made for.
pub(crate) struct UploadJournal {
    /// Format version (1).
    pub(crate) version: u32,
    /// BLAKE3 of the serialized upload plan (`upload_plan_fingerprint`); a
    /// mismatch rejects the journal.
    pub(crate) plan_fingerprint: String,
    #[serde(default)]
    /// Completed shards by shard index.
    pub(crate) completed: BTreeMap<u32, Shard>,
    #[serde(default)]
    /// Last update, Unix seconds.
    pub(crate) updated_unix: u64,
}
