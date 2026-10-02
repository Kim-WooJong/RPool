//! Upload preflight: checks the queue and target before an upload can start and
//! prepares the summary card's texts.

use super::state::{UploadItemStatus, UploadTargetMode};
use crate::erasure::validate_rs_counts;
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::models::PoolDefinition;
use crate::pool::validate_pool;
use crate::remote_root::remote_name;
use std::collections::BTreeSet;

/// Result of `evaluate`: whether the upload can start, summary labels, and the
/// problems found. Shown by `summary::show`; gates the start button.
#[derive(Debug, Clone)]
pub(crate) struct UploadPreflight {
    /// No issues: the upload can start.
    pub(crate) ready: bool,
    /// A batch runs or an item is uploading.
    pub(crate) in_progress: bool,
    /// Files line (pending / uploading counts and size).
    pub(crate) files_label: String,
    /// Target line (pool name or manual).
    pub(crate) target_label: String,
    /// Policy line (RS K+M, shard size, placement).
    pub(crate) policy_label: String,
    /// Storage line (number of encrypted destinations).
    pub(crate) destinations_label: String,
    /// Blocking problems; the first one is the start button's tooltip.
    pub(crate) issues: Vec<String>,
    /// Non-blocking notes.
    pub(crate) warnings: Vec<String>,
}

impl UploadPreflight {
    /// The first blocking issue, if any.
    pub(crate) fn first_issue(&self) -> Option<&str> {
        self.issues.first().map(String::as_str)
    }
}

/// Checks the queue (pending items, files still present) and the target, and
/// builds the summary. Called by `upload::show` every frame.
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
        issues.push(tr("Add one or more local files before starting the upload.").to_string());
    } else if pending == 0 && running == 0 {
        issues.push(tr("There are no pending files to upload.").to_string());
    }
    if missing_files > 0 {
        issues.push(trf(
            "{n} pending file(s) are no longer available at their selected paths.",
            &[("n", &missing_files)],
        ));
    }

    let files_label = if running > 0 {
        trf(
            "{running} uploading · {pending} pending · {size}",
            &[
                ("running", &running),
                ("pending", &pending),
                ("size", &crate::presentation::format_bytes(pending_bytes)),
            ],
        )
    } else if pending == 0 {
        tr("No pending files").to_string()
    } else {
        trf(
            "{pending} pending · {size}",
            &[
                ("pending", &pending),
                ("size", &crate::presentation::format_bytes(pending_bytes)),
            ],
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

/// `Err` with the first preflight issue; called by `batch::start_batch`.
pub(crate) fn validate_configuration(state: &GuiState) -> Result<(), String> {
    let report = evaluate(state);
    if let Some(issue) = report.first_issue() {
        return Err(issue.to_string());
    }
    Ok(())
}

/// Pool mode: the pool exists, is valid and writes only to crypt remotes.
/// Returns the target, policy and storage labels.
fn validate_pool_target(
    state: &GuiState,
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> (String, String, String) {
    let name = state.upload.pool_name.trim();
    if name.is_empty() {
        issues.push(tr("Select a storage pool first.").to_string());
        return (
            tr("Storage pool not selected").to_string(),
            tr("Pool policy").to_string(),
            tr("No destinations").to_string(),
        );
    }

    let Some(pool) = state.pool_definitions.get(name) else {
        issues.push(trf(
            "Storage pool '{name}' no longer exists.",
            &[("name", &name)],
        ));
        return (
            trf("Pool: {name}", &[("name", &name)]),
            tr("Unavailable").to_string(),
            tr("No destinations").to_string(),
        );
    };

    if let Err(error) = validate_pool(pool) {
        issues.push(trf(
            "Storage pool '{name}' is invalid: {error}",
            &[("name", &name), ("error", &format!("{error:#}"))],
        ));
    }
    validate_crypt_destinations(pool, &state.crypt_remotes, issues, warnings);

    (
        trf("Pool: {name}", &[("name", &name)]),
        pool_policy_label(pool),
        destination_count_label(pool.remotes.len()),
    )
}

/// Manual mode: at least one remote, valid shard size, workers and K+M, and
/// crypt-only remotes. Returns the target, policy and storage labels.
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
        issues.push(tr("Add at least one crypt destination for the manual upload.").to_string());
    }
    if let Err(error) = crate::models::shard_size::validate_shard_mib(state.settings.shard_mib) {
        issues.push(format!("{error}."));
    }
    if state.settings.workers == 0 {
        issues.push(tr("Workers must be greater than zero.").to_string());
    }
    if state.settings.parity_shards > 0 {
        if let Err(error) =
            validate_rs_counts(state.settings.data_shards, state.settings.parity_shards)
        {
            issues.push(trf(
                "Invalid Reed-Solomon policy: {error}",
                &[("error", &format!("{error:#}"))],
            ));
        }
    }

    validate_remote_names(&remotes, &state.crypt_remotes, issues, warnings);

    let policy_label = if state.settings.parity_shards > 0 {
        trf(
            "RS {k}+{m} · {mib} MiB shards",
            &[
                ("k", &state.settings.data_shards),
                ("m", &state.settings.parity_shards),
                ("mib", &state.settings.shard_mib),
            ],
        )
    } else {
        trf(
            "No EC · {mib} MiB shards",
            &[("mib", &state.settings.shard_mib)],
        )
    };

    (
        tr("Manual one-off").to_string(),
        policy_label,
        destination_count_label(remotes.len()),
    )
}

/// Checks the pool's remotes against the known crypt remotes.
fn validate_crypt_destinations(
    pool: &PoolDefinition,
    crypt_remotes: &[String],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    validate_remote_names(&pool.remotes, crypt_remotes, issues, warnings);
}

/// Adds an issue for remotes that are not known crypt remotes (case-insensitive
/// by remote name); only a warning when crypt discovery has no results yet.
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
            tr("Crypt remote discovery is not available yet. The storage layer will still enforce encryption before any write.")
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
        issues.push(trf(
            "These destinations are not recognized as encrypted rclone crypt remotes: {remotes}",
            &[("remotes", &unsafe_remotes.join(", "))],
        ));
    }
}

/// "RS K+M · N MiB shards · placement" (or "No EC …") for a pool.
fn pool_policy_label(pool: &PoolDefinition) -> String {
    if pool.parity_shards > 0 {
        trf(
            "RS {k}+{m} · {mib} MiB shards · {placement}",
            &[
                ("k", &pool.data_shards),
                ("m", &pool.parity_shards),
                ("mib", &pool.shard_size),
                ("placement", &pool.placement.label()),
            ],
        )
    } else {
        trf(
            "No EC · {mib} MiB shards · {placement}",
            &[
                ("mib", &pool.shard_size),
                ("placement", &pool.placement.label()),
            ],
        )
    }
}

/// "N encrypted destination(s)" text.
fn destination_count_label(count: usize) -> String {
    match count {
        0 => tr("No destinations").to_string(),
        1 => tr("1 encrypted destination").to_string(),
        value => trf("{n} encrypted destinations", &[("n", &value)]),
    }
}
