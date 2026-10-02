//! Provider (cloud account) operations: health checks, keep-alive, the
//! per-account limits view, shard draining between providers and the
//! interactive rclone connection setup used by the GUI.
/// Reachability, latency and quota checks for remotes.
mod health;
pub(crate) mod keepalive;
pub(crate) mod limits_view;
/// Moving an archive's shards from one provider to another (`provider drain`).
mod migrate;

pub(crate) use health::check_providers;
pub(crate) use migrate::drain_manifest;
#[cfg(test)]
pub(crate) use migrate::drain_manifest_with_storage;

#[cfg(test)]
pub(crate) use health::check_providers_with_admin;

pub(crate) mod onboarding;
