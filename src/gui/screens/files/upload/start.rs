//! Builds and starts the `rpool put` task for one queued file.

use super::state::UploadTargetMode;
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use std::ffi::OsString;
use std::path::Path;

/// Arguments of `rpool put <source>` with the pool or manual target flags,
/// plus `--id` when a single file has a typed archive ID. Called by
/// `batch::start_batch` for every queued file.
pub(crate) fn upload_args(state: &GuiState, source: &Path) -> Result<Vec<OsString>, String> {
    let mut args: Vec<OsString> = vec![OsString::from("put"), source.as_os_str().to_os_string()];
    match state.upload.target_mode {
        UploadTargetMode::Pool => append_pool_target(state, &mut args)?,
        UploadTargetMode::Manual => append_manual_target(state, &mut args)?,
    }

    if state.upload.items.len() == 1 && !state.upload.archive_id.trim().is_empty() {
        args.push(OsString::from("--id"));
        args.push(OsString::from(state.upload.archive_id.trim()));
    }
    Ok(args)
}

/// Pool target: `--pool <name> --workers N` and, when set, the one-off policy
/// override flags. Errors when no pool is selected.
fn append_pool_target(state: &GuiState, args: &mut Vec<OsString>) -> Result<(), String> {
    let pool = state.upload.pool_name.trim();
    if pool.is_empty() {
        return Err(tr("Select a storage pool first.").to_string());
    }
    args.push(OsString::from("--pool"));
    args.push(OsString::from(pool));
    // Shard transfers are this PC's setting (Settings › Network & transfers).
    args.push(OsString::from("--workers"));
    args.push(OsString::from(state.settings.workers.to_string()));
    if let Some(policy) = &state.upload.policy_override {
        args.extend(policy.args());
    }
    Ok(())
}

/// Manual target: one `--remote` per configured remote plus the shard, worker,
/// placement, retry and K+M flags from the settings. Errors without a remote.
fn append_manual_target(state: &GuiState, args: &mut Vec<OsString>) -> Result<(), String> {
    if !state
        .settings
        .remotes
        .iter()
        .any(|remote| !remote.trim().is_empty())
    {
        return Err(tr("Add at least one cloud destination.").to_string());
    }

    for remote in &state.settings.remotes {
        if !remote.trim().is_empty() {
            args.push(OsString::from("--remote"));
            args.push(OsString::from(remote.trim()));
        }
    }

    args.extend([
        OsString::from("--shard-mib"),
        OsString::from(state.settings.shard_mib.to_string()),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
        OsString::from("--placement"),
        OsString::from(state.settings.placement.cli_value()),
        OsString::from("--retries"),
        OsString::from(state.settings.retries.to_string()),
        OsString::from("--data-shards"),
        OsString::from(state.settings.data_shards.to_string()),
        OsString::from("--parity-shards"),
        OsString::from(state.settings.parity_shards.to_string()),
    ]);
    Ok(())
}
