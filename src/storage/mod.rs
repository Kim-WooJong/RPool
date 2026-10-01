pub(crate) mod account;
pub(crate) mod admin;
pub(crate) mod capabilities;
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
pub(crate) mod traits;
pub(crate) mod transfer_budget;
pub(crate) mod verified;
pub(crate) mod writer;

pub(crate) use data_upload::upload_one_data_shard;
pub(crate) use remote_config::list_crypt_remotes;

#[cfg(all(test, unix))]
mod verified_tests;
#[cfg(test)]
mod writer_tests;
