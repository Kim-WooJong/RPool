//! Resume journals: which shards an interrupted upload already stored and
//! which data shards an interrupted restore already wrote, so a rerun skips
//! verified work. Used by `put`, `get` and mount uploads.

/// Restore resume state (`get`).
mod restore;
/// Upload journal (`put`, mount uploads).
mod upload;

pub(crate) use restore::{persist_restore_state, validate_restore_state};
pub(crate) use upload::{
    load_or_create_upload_journal, record_upload_shard, validate_upload_journal,
};
