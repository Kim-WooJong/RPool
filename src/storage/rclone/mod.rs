//! Single owning rclone data/admin subprocess adapter. Legacy raw addresses and
//! typed keys share the same primitives, classification and crypt write gate.
#[cfg(all(test, unix))]
#[path = "account_rclone_tests.rs"]
mod account_rclone_tests;
mod bwlimit_schedule;
#[cfg(all(test, unix))]
#[path = "copy_tests.rs"]
mod copy_tests;
mod daemon;
#[cfg(all(test, unix))]
#[path = "daemon_tests.rs"]
mod daemon_tests;
mod dirs;
mod http;
mod limit;
mod pacer;
mod process;
mod stall;
#[cfg(test)]
mod tests;
pub(crate) mod traffic;
#[cfg(all(test, unix))]
#[path = "traffic_rclone_tests.rs"]
mod traffic_rclone_tests;

pub(crate) use daemon::ShutdownGuard as DaemonShutdownGuard;

use crate::storage::capabilities::{BackendCapabilities, Capability, ConsistencyScope};
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

mod admin;
mod backend;
mod context;
mod copy;
mod parse;
mod read;
mod sink;
mod write;

pub(crate) use backend::RcloneBackend;
pub(crate) use parse::{
    join_base, parse_backend_features, parse_cryptdecode, parse_object_hash, preferred_hashes,
    remote_name, write_account, write_base, BackendFeatures, CryptCopyCapabilities,
};
use sink::{AdminOutputCap, BoundedVec, Counted, RangeSink};
use write::permit;

const ADMIN_LIMIT: usize = 8 * 1024 * 1024;
const LIST_LIMIT: usize = 1 << 30;
/// Extra attempts for a mutation the provider rejected before performing it.
const REJECTED_RETRIES: u32 = 3;
const REJECTED_BACKOFF_MS: u64 = if cfg!(test) { 20 } else { 2000 };

#[derive(Clone)]
pub(crate) enum ConfigSelection {
    Inherited,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Explicit config selection remains available to opt-in backend callers"
        )
    )]
    File(PathBuf),
}

#[derive(Clone)]
pub(crate) struct RcloneContext {
    executable: PathBuf,
    config: ConfigSelection,
    environment: Vec<(OsString, OsString)>,
    /// Read-only calls may use the shared `rclone rcd` daemon. Off in unit
    /// tests unless a test opts in, so tests never start real daemons.
    daemon_allowed: bool,
    /// (base remote name, crypt remote name): traffic on the base counts for
    /// the crypt remote (native crypt writes address the base).
    traffic_alias: Option<(String, String)>,
}

fn invalid(detail: &str) -> StorageError {
    StorageError::invalid_input(detail)
}
