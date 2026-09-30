//! Protocol-independent filesystem core over `VirtualDrive` (native mount M3).
//!
//! Frontends (DAV now; WinFsp, FUSE and NFS later) translate their requests into
//! these handle operations. Contract:
//! - `write_at` and `truncate` are volatile. `fsync`/`flush`/`freeze` and the
//!   last write handle's `release` seal the
//!   file's write generation, and that seal is the local durability
//!   acknowledgement. Cloud upload is started later by `sync` and never gates it.
//! - All write handles of one file share one generation (inode semantics).
//! - A read-only handle opened while the file has no unsealed writes reads an
//!   immutable snapshot for its whole life. Other handles follow this core's
//!   seals of the same file.
//! - Unlink/rename-over leaves open handles working; unsealed writes of an
//!   unlinked file are never acknowledged and are discarded at last release.
//! - Nothing here is persisted: file identity is per session.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "each OS builds only its own native frontend")
)]
mod core;
#[cfg(test)]
mod crash_tests;
mod durability;
mod error;
mod generation;
mod handles;
mod identity;
mod namespace_ops;
#[cfg(test)]
mod peer_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod trace_tests;

#[allow(unused_imports, reason = "API for the frontends that adopt the core")]
pub(crate) use self::core::{Access, Attr, FsCore};
#[allow(unused_imports, reason = "API for the frontends that adopt the core")]
pub(crate) use error::{FsError, FsResult};
#[allow(unused_imports, reason = "API for the frontends that adopt the core")]
pub(crate) use handles::HandleId;
#[allow(unused_imports, reason = "API for the frontends that adopt the core")]
pub(crate) use identity::FileId;
