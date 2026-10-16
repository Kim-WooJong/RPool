//! Provider storage manager (`rpool provider storage list|clean`, GUI
//! Storage › Providers › "Manage storage…"): everything RPool keeps on one
//! crypt remote, grouped by owner, and a fail-closed cleanup of the data no
//! configured pool needs any more.
//!
//! - `layout`: pure parser of one recursive listing into units (archive
//!   folders, metadata generations, migration journals, speed-test runs…).
//! - `classify`: pure decision which group every unit belongs to and which
//!   groups may be deleted (old drive generations, foreign pools, speed-test
//!   leftovers); everything else is kept. Any unreadable source makes the
//!   whole view uncertain: nothing is deletable.
//! - `observe`: the read-only cloud side (listings of every account of every
//!   relevant pool, drive metadata of every generation, migrations, inventory
//!   and manifests) feeding `classify`.
//! - `journal` / `execute`: marks with a grace period published on the
//!   provider (`.rpool-sync/storage-cleanup-v1/`), re-check and deletion
//!   (same rules as `drive_history::cleanup`); `live`: the cloud `StorageIo`.
/// Pure grouping of units into owner groups.
pub(crate) mod classify;
/// Marks, grace period, re-check and deletion over a `StorageIo`.
pub(crate) mod execute;
/// The mark journal on the provider.
pub(crate) mod journal;
/// Pure parser of a provider listing into units.
pub(crate) mod layout;
/// Cloud implementation of `StorageIo`.
mod live;
/// Serializable reports (`--json`, GUI).
pub(crate) mod model;
/// Read-only observation of the cloud.
mod observe;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod rclone_tests;

use crate::prelude::*;
use execute::StorageIo;

/// Lists the provider `remote` (a crypt remote, `name` or `name:path`) with
/// its groups and pending marks. Read-only. Called by
/// `commands::provider::storage`.
pub(crate) fn list(rclone: &str, remote: &str) -> Result<model::StorageView> {
    let io = live::LiveIo::open(rclone, remote, 1)?;
    let classified = io.observe()?;
    Ok(execute::view(
        &classified,
        io.records(),
        io.grace_days(),
        io.undecryptable(),
    ))
}

/// Previews or runs a cleanup of the selected groups on `remote` (see
/// `execute::run`). Called by `commands::provider::storage`.
pub(crate) fn clean(
    rclone: &str,
    remote: &str,
    request: &execute::Request,
) -> Result<model::CleanReport> {
    let io = live::LiveIo::open(rclone, remote, request.workers)?;
    execute::run(&io, request)
}
