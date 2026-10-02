//! Archive integrity maintenance: probe every shard of a manifest (scrub),
//! judge each coding group's recoverability, rebuild bad shards and keep the
//! last integrity snapshot. Used by `scrub`, `repair`, `get` and migrations.

/// Group status from shard probes.
mod group_health;
/// Counts of degraded and unrecoverable groups.
mod recoverability;
/// Rebuilding and re-uploading bad shards.
mod repair;
/// Probing all shards into a scrub report.
mod scan;
/// The last integrity snapshot on disk.
mod snapshot_store;

pub(crate) use group_health::analyze_groups;
pub(crate) use recoverability::group_recoverability;
pub(crate) use repair::{reconstruct_group_files, repair_manifest_with_storage};
pub(crate) use scan::scan_manifest_with_storage;
pub(crate) use snapshot_store::{load_integrity_snapshot, save_integrity_snapshot};
