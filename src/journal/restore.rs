//! Restore resume state for `get`.

use crate::manifest::data_shards;
use crate::prelude::*;
use crate::utils::{hash_file_range, now_unix, save_json_atomic};

/// Re-checks completed shards against the partly written `output` (hash of each
/// shard's byte range) and drops unknown or mismatching ones, then saves the
/// state. Returns how many entries were dropped. Called by `commands::get`.
pub(crate) fn validate_restore_state(
    manifest: &Manifest,
    output: &Path,
    state: &mut ResumeState,
    state_path: &Path,
) -> Result<usize> {
    let valid_indexes: BTreeSet<u32> = data_shards(manifest)
        .iter()
        .map(|shard| shard.index)
        .collect();
    let unknown = state
        .completed
        .iter()
        .filter(|index| !valid_indexes.contains(index))
        .count();
    state
        .completed
        .retain(|index| valid_indexes.contains(index));
    let mut invalid = Vec::new();
    for shard in data_shards(manifest) {
        if !state.completed.contains(&shard.index) {
            continue;
        }
        match hash_file_range(output, shard.offset, shard.size) {
            Ok(hash) if hash == shard.blake3 => {}
            _ => invalid.push(shard.index),
        }
    }
    for index in &invalid {
        state.completed.remove(index);
    }
    state.version = 2;
    state.original_size = manifest.original_size;
    state.updated_unix = now_unix();
    save_json_atomic(state_path, state)?;
    Ok(invalid.len() + unknown)
}

/// Saves the restore state (format version 2) with a fresh timestamp.
pub(crate) fn persist_restore_state(path: &Path, state: &mut ResumeState) -> Result<()> {
    state.version = 2;
    state.updated_unix = now_unix();
    save_json_atomic(path, state)
}
