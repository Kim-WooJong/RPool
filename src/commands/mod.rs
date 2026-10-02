//! CLI command handlers. `application::dispatch` calls these after parsing; most
//! are thin wrappers that print results, while `put`, `get`, `verify`, `scrub` and
//! `repair` also expose `*_with_storage` variants used by the mount and tests.
/// `export` / `import` handlers.
pub(crate) mod config_sync;
/// `doctor` handler.
mod doctor;
/// `get`: restore a file from its manifest.
mod get;
/// `history` handlers.
pub(crate) mod history;
/// `inventory` handlers.
pub(crate) mod inventory;
/// `manifest` replica handlers.
pub(crate) mod manifest_ops;
/// `pool` handlers (list, show, set, remove, browse, migrate, compact).
pub(crate) mod pool;
/// `provider` handlers (limits, keepalive, health, drain).
pub(crate) mod provider;
/// `put`: shard, encode and upload a file.
mod put;
/// `remote-root` handlers.
pub(crate) mod remote_root;
/// `repair`: rebuild bad shards from parity.
mod repair;
/// `scrub`: scan shards and optionally repair.
mod scrub;
/// `status`: shard availability and recoverability.
mod status;
/// `usage`: provider quota table.
mod usage;
/// `verify`: shard existence/size or full BLAKE3 check.
mod verify;

pub(crate) use doctor::doctor;
pub(crate) use get::{get, get_with_storage};
pub(crate) use put::put;
pub(crate) use repair::repair;
pub(crate) use scrub::scrub;
pub(crate) use status::status;
pub(crate) use usage::usage;
pub(crate) use verify::{reverify_with_storage, verify, verify_with_storage};

pub(crate) use put::{put_sealed_with_storage, put_with_storage};

#[cfg(test)]
pub(crate) use repair::repair_with_storage;
#[cfg(test)]
pub(crate) use scrub::scrub_with_storage;
