//! In-process API for the GUI (blocking: call off the UI thread). Same
//! routing and results as `rpool drive … --json`: a running mount of the
//! pool answers, else the given workspace is opened, else the cloud is read.
//! Notes (degraded information) are returned beside the value.
#![allow(dead_code, reason = "called by the GUI drive history screens")]
use super::model::{Retention, RollbackPlan, TrashEntry, VersionEntry};
use super::ops::{Op, PurgeReport, RestoreReport};
use crate::prelude::*;

fn run<T: serde::de::DeserializeOwned>(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    op: Op,
) -> Result<(T, Vec<String>)> {
    let (value, notes) = super::dispatch::run(rclone, pool, workspace, &op)?;
    Ok((serde_json::from_value(value)?, notes))
}

pub(crate) fn trash_list(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
) -> Result<(Vec<TrashEntry>, Vec<String>)> {
    run(rclone, pool, workspace, Op::TrashList)
}
pub(crate) fn trash_restore(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    ids: Vec<String>,
    to: Option<String>,
    into: Option<String>,
) -> Result<(RestoreReport, Vec<String>)> {
    run(rclone, pool, workspace, Op::TrashRestore { ids, to, into })
}
/// `confirm: false` previews. `all`: empty the trash.
pub(crate) fn trash_purge(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    ids: Vec<String>,
    expired: bool,
    all: bool,
    confirm: bool,
) -> Result<(PurgeReport, Vec<String>)> {
    run(
        rclone,
        pool,
        workspace,
        Op::TrashPurge {
            ids,
            expired,
            all,
            confirm,
        },
    )
}
pub(crate) fn versions_list(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    path: &str,
) -> Result<(Vec<VersionEntry>, Vec<String>)> {
    run(
        rclone,
        pool,
        workspace,
        Op::VersionsList { path: path.into() },
    )
}
pub(crate) fn versions_restore(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    path: &str,
    id: &str,
    as_copy: bool,
) -> Result<(RestoreReport, Vec<String>)> {
    run(
        rclone,
        pool,
        workspace,
        Op::VersionsRestore {
            path: path.into(),
            id: id.into(),
            as_copy,
        },
    )
}
/// Preview (`confirm: false`) or apply a rollback of `path` (`/` = drive).
pub(crate) fn rollback(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    path: &str,
    at_unix: u64,
    confirm: bool,
) -> Result<(RollbackPlan, Vec<String>)> {
    run(
        rclone,
        pool,
        workspace,
        Op::Rollback {
            path: path.into(),
            at: at_unix,
            confirm,
        },
    )
}
pub(crate) fn retention(pool: &str) -> Result<Retention> {
    super::retention::load(pool)
}
pub(crate) fn set_retention(pool: &str, value: Retention) -> Result<Retention> {
    super::retention::update(
        pool,
        Some(value.trash_days),
        Some(value.keep_versions),
        Some(value.version_days),
    )
}
/// `rpool drive cleanup` (preview unless `request.confirm`).
pub(crate) fn cleanup(
    rclone: &str,
    pool: &str,
    workspace: Option<&Path>,
    request: super::cleanup::Request,
) -> Result<(super::model::CleanupReport, Vec<String>)> {
    run(rclone, pool, workspace, Op::Cleanup(request))
}
