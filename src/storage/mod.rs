//! Storage layer: backends (rclone, native crypt), typed errors and
//! capabilities, per-account limits/admin, readers/writers and upload sessions.
//!
//! Higher layers (archive, mount, pool) go through `reader`/`writer`/`traits`
//! rather than calling rclone directly.

pub(crate) mod account;
pub(crate) mod admin;
pub(crate) mod capabilities;
/// Single data-shard upload helper (re-exported below).
mod data_upload;
pub(crate) mod error;
// Synthetic contract backend; not a production archive write destination.
#[cfg(test)]
pub(crate) mod memory;
pub(crate) mod native_crypt;
pub(crate) mod rclone;
pub(crate) mod reader;
pub(crate) mod reference;
pub(crate) mod registry;
mod remote_config;
pub(crate) mod scheduler;
pub(crate) mod source;
pub(crate) mod stored_hash;
pub(crate) mod traits;
pub(crate) mod transfer_budget;
pub(crate) mod upload_session;
pub(crate) mod verified;
pub(crate) mod writer;

pub(crate) use data_upload::upload_one_data_shard;
pub(crate) use remote_config::list_crypt_remotes;

#[cfg(all(test, unix))]
mod verified_tests;
#[cfg(test)]
mod writer_tests;
