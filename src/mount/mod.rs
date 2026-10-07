//! Mounted virtual drive and everything behind it: pool-sync metadata (v6
//! events, checkpoints, compaction), the shard cache, local spool, upload
//! workers, OS frontends (WinFsp / FUSE / macFUSE / WebDAV), pool transitions
//! and account recovery. Entry point: `run` for `rpool mount`.

mod account_recovery;
pub(crate) mod adapter;
pub(crate) mod adoption_fence;
pub(crate) mod adoption_workspace;
pub(crate) mod cache_recovery;
pub(crate) mod capacity;
pub(crate) mod capacity_balance;
mod crash;
mod dav;
pub(crate) mod drive_generation_read;
pub(crate) mod drive_generation_write;
pub(crate) mod drive_references;
mod frontend;
pub(crate) mod fs_core;
#[cfg(target_os = "macos")]
pub(crate) use frontend::macfuse_installed;
#[cfg(test)]
mod add_accounts_tests;
pub(crate) mod history_bridge;
mod incremental;
pub(crate) mod layout_refresh;
mod lifecycle;
mod maintenance;
pub(crate) mod metadata_backfill;
pub(crate) mod metadata_browse;
mod metadata_cache;
mod metadata_checkpoint;
mod metadata_checkpoint_model;
pub(crate) mod metadata_compaction;
mod metadata_dir;
pub(crate) mod metadata_limits;
pub(crate) mod metadata_pool;
#[cfg(test)]
mod metadata_tests;
pub(crate) mod namespace;
mod native_ancestry;
pub(crate) mod peer_projection;
pub(crate) mod pool_sync;
mod pool_transition;
pub(crate) mod rclone_import;
mod shard_cache;
mod shared_model;
pub(crate) mod shutdown_signal;
mod spool;
pub(crate) mod workspace_backups;
/// A pack member's position, used by drive migration (`migration::drive_model`).
pub(crate) use shared_model::PackSlice;
/// Synthetic metadata for read-only listing tests outside `mount`.
#[cfg(test)]
pub(crate) use shared_model::{Content as TestContent, Event as TestEvent};
mod publisher;
mod shared_transport;
mod upload;
mod upload_worker;
/// The mounted drive itself (`VirtualDrive`): open/read/write/rename,
/// upload queue and rounds, pool sync, capacity and recovery.
pub(crate) mod virtual_drive;

use anyhow::Result;

/// `rpool mount`: the virtual drive with automatic pool sync (v6), or one of
/// its offline actions (pool change, account recovery, import, sync-only...).
pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    if args.recovery_reprocess_plan.is_some()
        && args.account_recovery_from.is_none()
        && !args.apply_pool_changes
    {
        anyhow::bail!("Reprocess plan requires account recovery or --apply-pool-changes");
    }
    if !args.recovery_skip_remote.is_empty() && args.account_recovery_from.is_none() {
        // clap cannot enforce this `requires` against the conflicting modes.
        anyhow::bail!("--recovery-skip-remote requires --account-recovery-from");
    }
    if args.apply_pool_changes {
        return pool_transition::run(rclone, &args);
    }
    let _transition_lock = pool_transition::lock_workspace_transition(&args.workspace)?;
    pool_transition::assert_no_incomplete_transition(&args.workspace)?;
    if args.account_recovery_from.is_some() {
        return account_recovery::run(rclone, &args);
    }
    crate::storage::admin::domains::update(&args.capacity_domain, &args.failure_domain)?;
    virtual_drive::run(rclone, args)
}

#[cfg(test)]
mod native_sync_tests;
#[cfg(test)]
mod virtual_tests;
