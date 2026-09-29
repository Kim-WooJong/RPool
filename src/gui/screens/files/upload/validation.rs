use super::state::{UploadItemStatus, UploadTargetMode};
use crate::erasure::validate_rs_counts;
use crate::gui::state::GuiState;
use crate::models::PoolDefinition;
use crate::pool::validate_pool;
use crate::remote_root::remote_name;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub(crate) struct UploadPreflight {
    pub(crate) ready: bool,
    pub(crate) in_progress: bool,
    pub(crate) files_label: String,
    pub(crate) target_label: String,
    pub(crate) policy_label: String,
    pub(crate) destinations_label: String,
    pub(crate) issues: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

impl UploadPreflight {
    pub(crate) fn first_issue(&self) -> Option<&str> {
        self.issues.first().map(String::as_str)
    }
}

pub(crate) fn evaluate(state: &GuiState) -> UploadPreflight {
    let mut issues = Vec::new();
    let mut warnings = Vec::new();

    let pending = state.upload.pending_count();
    let running = state
        .upload
        .items
        .iter()
        .filter(|item| item.status == UploadItemStatus::Running)
        .count();
    let pending_bytes: u64 = state
        .upload
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.status,
                UploadItemStatus::Pending | UploadItemStatus::Running
            )
        })
        .filter_map(|item| item.size)
        .sum();
    let missing_files = state
        .upload
        .items
        .iter()
        .filter(|item| item.status == UploadItemStatus::Pending && !item.path.is_file())
        .count();

    if state.upload.items.is_empty() {
        issues.push("Add one or more local files before starting the upload.".to_string());
    } else if pending == 0 && running == 0 {
        issues.push("There are no pending files to upload.".to_string());
    }
    if missing_files > 0 {
        issues.push(format!(
            "{missing_files} pending file(s) are no longer available at their selected paths."
        ));
    }

    let files_label = if running > 0 {
        format!(
            "{running} uploading · {pending} pending · {}",
            crate::presentation::format_bytes(pending_bytes)
        )
    } else if pending == 0 {
        "No pending files".to_string()
    } else {
        format!(
            "{pending} pending · {}",
            crate::presentation::format_bytes(pending_bytes)
        )
    };

    let (target_label, policy_label, destinations_label) = match state.upload.target_mode {
        UploadTargetMode::Pool => validate_pool_target(state, &mut issues, &mut warnings),
        UploadTargetMode::Manual => validate_manual_target(state, &mut issues, &mut warnings),
    };

    UploadPreflight {
        ready: issues.is_empty(),
        in_progress: state.upload.batch_active || running > 0,
        files_label,
        target_label,
        policy_label,
        destinations_label,
        issues,
        warnings,
    }
}

pub(crate) fn validate_configuration(state: &GuiState) -> Result<(), String> {
    let report = evaluate(state);
    if let Some(issue) = report.first_issue() {
        return Err(issue.to_string());
    }
    Ok(())
}

fn validate_pool_target(
    state: &GuiState,
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> (String, String, String) {
    let name = state.upload.pool_name.trim();
    if name.is_empty() {
        issues.push("Select a storage pool first.".to_string());
        return (
            "Storage pool not selected".to_string(),
            "Pool policy".to_string(),
            "No destinations".to_string(),
        );
    }

    let Some(pool) = state.pool_definitions.get(name) else {
        issues.push(format!("Storage pool '{name}' no longer exists."));
        return (
            format!("Pool: {name}"),
            "Unavailable".to_string(),
            "No destinations".to_string(),
        );
    };

    if let Err(error) = validate_pool(pool) {
        issues.push(format!("Storage pool '{name}' is invalid: {error:#}"));
    }
    validate_crypt_destinations(pool, &state.crypt_remotes, issues, warnings);

    (
        format!("Pool: {name}"),
        pool_policy_label(pool),
        destination_count_label(pool.remotes.len()),
    )
}

fn validate_manual_target(
    state: &GuiState,
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> (String, String, String) {
    let remotes: Vec<String> = state
        .settings
        .remotes
        .iter()
        .filter_map(|remote| {
            let value = remote.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect();

    if remotes.is_empty() {
        issues.push("Add at least one crypt destination for the manual upload.".to_string());
    }
    if let Err(error) = crate::models::shard_size::validate_shard_mib(state.settings.shard_mib) {
        issues.push(format!("{error}."));
    }
    if state.settings.workers == 0 {
        issues.push("Workers must be greater than zero.".to_string());
    }
    if state.settings.parity_shards > 0 {
        if let Err(error) =
            validate_rs_counts(state.settings.data_shards, state.settings.parity_shards)
        {
            issues.push(format!("Invalid Reed-Solomon policy: {error:#}"));
        }
    }

    validate_remote_names(&remotes, &state.crypt_remotes, issues, warnings);

    let policy_label = if state.settings.parity_shards > 0 {
        format!(
            "RS {}+{} · {} MiB shards",
            state.settings.data_shards, state.settings.parity_shards, state.settings.shard_mib
        )
    } else {
        format!("No EC · {} MiB shards", state.settings.shard_mib)
    };

    (
        "Manual one-off".to_string(),
        policy_label,
        destination_count_label(remotes.len()),
    )
}

fn validate_crypt_destinations(
    pool: &PoolDefinition,
    crypt_remotes: &[String],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    validate_remote_names(&pool.remotes, crypt_remotes, issues, warnings);
}

fn validate_remote_names(
    remotes: &[String],
    crypt_remotes: &[String],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if remotes.is_empty() {
        return;
    }
    if crypt_remotes.is_empty() {
        warnings.push(
            "Crypt remote discovery is not available yet. The storage layer will still enforce encryption before any write."
                .to_string(),
        );
        return;
    }

    let configured: BTreeSet<String> = crypt_remotes
        .iter()
        .filter_map(|remote| remote_name(remote).ok())
        .map(|name| name.to_ascii_lowercase())
        .collect();

    let mut unsafe_remotes = Vec::new();
    for remote in remotes {
        match remote_name(remote) {
            Ok(name) if configured.contains(&name.to_ascii_lowercase()) => {}
            Ok(name) => unsafe_remotes.push(format!("{name}:")),
            Err(_) => unsafe_remotes.push(remote.clone()),
        }
    }
    unsafe_remotes.sort();
    unsafe_remotes.dedup();

    if !unsafe_remotes.is_empty() {
        issues.push(format!(
            "These destinations are not recognized as encrypted rclone crypt remotes: {}",
            unsafe_remotes.join(", ")
        ));
    }
}

fn pool_policy_label(pool: &PoolDefinition) -> String {
    if pool.parity_shards > 0 {
        format!(
            "RS {}+{} · {} MiB shards · {}",
            pool.data_shards,
            pool.parity_shards,
            pool.shard_size,
            pool.placement.label()
        )
    } else {
        format!(
            "No EC · {} MiB shards · {}",
            pool.shard_size,
            pool.placement.label()
        )
    }
}

fn destination_count_label(count: usize) -> String {
    match count {
        0 => "No destinations".to_string(),
        1 => "1 encrypted destination".to_string(),
        value => format!("{value} encrypted destinations"),
    }
}
