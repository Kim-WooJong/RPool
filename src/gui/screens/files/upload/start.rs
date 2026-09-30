use super::state::UploadTargetMode;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use std::ffi::OsString;
use std::path::Path;

pub(crate) fn start_upload_path(
    state: &GuiState,
    task: &mut TaskRunner,
    source: &Path,
) -> Result<(), String> {
    let mut args: Vec<OsString> = vec![OsString::from("put"), source.as_os_str().to_os_string()];
    match state.upload.target_mode {
        UploadTargetMode::Pool => append_pool_target(state, &mut args)?,
        UploadTargetMode::Manual => append_manual_target(state, &mut args)?,
    }

    if state.upload.items.len() == 1 && !state.upload.archive_id.trim().is_empty() {
        args.push(OsString::from("--id"));
        args.push(OsString::from(state.upload.archive_id.trim()));
    }

    let source_name = source
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string());
    task.start_rpool(
        format!("Upload {source_name}"),
        &state.settings.rclone,
        args,
    )
}

fn append_pool_target(state: &GuiState, args: &mut Vec<OsString>) -> Result<(), String> {
    let pool = state.upload.pool_name.trim();
    if pool.is_empty() {
        return Err("Select a storage pool first.".to_string());
    }
    args.push(OsString::from("--pool"));
    args.push(OsString::from(pool));
    if let Some(policy) = &state.upload.policy_override {
        args.extend(policy.args());
    }
    Ok(())
}

fn append_manual_target(state: &GuiState, args: &mut Vec<OsString>) -> Result<(), String> {
    if !state
        .settings
        .remotes
        .iter()
        .any(|remote| !remote.trim().is_empty())
    {
        return Err("Add at least one cloud destination.".to_string());
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
