mod content_root;
mod fingerprint;
mod load;
mod query;
mod recover;
mod replica_verify;
mod replicate;
mod targets;
mod validate;

pub(crate) use content_root::{content_root_v1, content_root_v2};
pub(crate) use fingerprint::manifest_fingerprint;
pub(crate) use load::{
    load_manifest, load_manifest_bytes_with_storage, load_manifest_with_storage,
};
pub(crate) use query::{coding_group_count, data_shards};
pub(crate) use recover::recover_manifest_with_storage;
pub(crate) use replica_verify::verify_manifest_replicas_with_storage;
pub(crate) use replicate::{
    replicate_manifest_bytes_with_storage, replicate_manifest_with_storage,
};
pub(crate) use targets::manifest_remotes;
pub(crate) use validate::validate_manifest;

#[cfg(test)]
mod compatibility_tests;
