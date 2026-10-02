//! Archive manifests: the JSON description of an archive's shards, coding and
//! placement. Loading, validation, fingerprints, replication to every used
//! remote, replica verification and recovery from remote replicas.

/// Content-root checksums (v1, v2).
mod content_root;
/// Fingerprint of a manifest for replica comparison.
mod fingerprint;
/// Loading from a local file or remote.
mod load;
/// Shard queries (data shards, group count).
mod query;
/// Recovering a manifest from remote replicas.
mod recover;
/// Verifying remote manifest replicas.
mod replica_verify;
/// Writing manifest replicas to remotes.
mod replicate;
/// Which remotes a manifest is stored on and published to.
mod targets;
/// Structural and checksum validation.
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
pub(crate) use targets::{manifest_remotes, publication_remotes};
pub(crate) use validate::validate_manifest;

#[cfg(test)]
mod compatibility_tests;
