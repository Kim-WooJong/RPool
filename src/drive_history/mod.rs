//! Trash, file versions and rollback for a pool's drive.
//!
//! `rpool drive trash list|restore|purge|empty`, `rpool drive versions
//! list|restore` and `rpool drive rollback` (preview by default) work on a
//! pool's drive (v6 events or v7 snapshots) and never destroy history:
//! restoring or rolling back publishes new revisions. Only `trash purge` /
//! `trash empty` (and expiry after the retention period) make a deleted
//! file's data eligible for removal. `--json` prints the [`model`] types;
//! the GUI reads those.
#![allow(dead_code)] // Contract stub: remove once CLI and GUI use it.
pub(crate) mod model;
