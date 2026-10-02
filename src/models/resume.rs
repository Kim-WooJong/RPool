//! Resume state of an interrupted `get` ([`ResumeState`]); validated by
//! `journal::restore` before reuse.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Data shards already written to the partial output file.
pub(crate) struct ResumeState {
    #[serde(default)]
    /// Format version (0 in old files).
    pub(crate) version: u32,
    /// Fingerprint of the manifest being restored; a mismatch discards the state.
    pub(crate) manifest_fingerprint: String,
    #[serde(default)]
    /// Expected output size, bytes (0 in old files).
    pub(crate) original_size: u64,
    /// Indexes of data shards already written.
    pub(crate) completed: BTreeSet<u32>,
    #[serde(default)]
    /// Last update, Unix seconds.
    pub(crate) updated_unix: u64,
}
