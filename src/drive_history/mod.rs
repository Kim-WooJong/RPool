//! Trash, file versions and rollback for a pool's drive.
//!
//! `rpool drive trash list|restore|purge|empty`, `rpool drive versions
//! list|restore` and `rpool drive rollback` (preview by default) work on a
//! pool's drive (v6 events) and never destroy history:
//! restoring or rolling back publishes new revisions. Only `trash purge` /
//! `trash empty` (and expiry after the retention period) make a deleted
//! file's data eligible for removal. `--json` prints the [`model`] types;
//! the GUI reads those (or calls [`api`] in process).
//!
//! Design: `docs/DRIVE_HISTORY_DESIGN.md`. Layout:
//! - `graph`: normalized revision graph; `source_v6` builds it,
//!   `times` (record ModTimes) and `marks` (published purge marks) feed it.
//! - `trash`, `versions`, `rollback`: pure planning over the graph;
//!   `restore`: planned actions; `retention`: settings and what they keep.
//! - `load`, `apply`, `ops`, `dispatch`, `request`: running a request in a
//!   mount, a workspace or from the cloud. `command`, `time_arg`: CLI.
pub(crate) mod api;
mod apply;
pub(crate) mod command;
pub(crate) mod dispatch;
mod graph;
mod load;
pub(crate) mod marks;
pub(crate) mod model;
pub(crate) mod ops;
mod request;
mod restore;
pub(crate) mod retention;
mod rollback;
mod source_v6;
mod time_arg;
mod times;
mod trash;
mod versions;

#[cfg(test)]
mod drive_tests;
#[cfg(test)]
mod rclone_tests;
