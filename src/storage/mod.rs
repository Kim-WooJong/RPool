pub(crate) mod admin;
pub(crate) mod capabilities;
mod data_upload;
pub(crate) mod error;
// Synthetic contract backend; not a production archive write destination.
#[cfg(test)]
pub(crate) mod memory;
// Unix-only synthetic prototype. Windows reparse-safe support is not implemented.
#[cfg(all(test, unix))]
pub(crate) mod local;
pub(crate) mod rclone;
pub(crate) mod reader;
pub(crate) mod reference;
pub(crate) mod registry;
mod remote_config;
pub(crate) mod scheduler;
pub(crate) mod source;
pub(crate) mod traits;
pub(crate) mod writer;

pub(crate) use data_upload::upload_one_data_shard;
pub(crate) use remote_config::list_crypt_remotes;

#[cfg(test)]
mod writer_tests;

// Default-off, Memory-only synthetic OpenDAL; no production native route.
#[cfg(all(test, feature = "opendal-prototype"))]
pub(crate) mod opendal;
