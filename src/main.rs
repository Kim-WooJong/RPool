//! `rpool`: stores files as Reed-Solomon shards across rclone crypt remotes,
//! with a CLI, an egui GUI and a mountable virtual drive. `main` only installs
//! the rclone daemon shutdown guard and hands over to [`application::run`].
// Every item carries a rustdoc comment (what it does, who uses it);
// `cargo clippy` fails on a missing one. Tests are exempt.
#![cfg_attr(not(test), warn(clippy::missing_docs_in_private_items))]

// The print macros below shadow std's in the whole crate (textual scope, so
// they must stay above every `mod`). std's versions panic once stdout or
// stderr is closed (GUI gone, terminal closed); these ignore the error, see
// `utils::console`. Tests keep std's macros so libtest still captures output.
/// `println!` that never panics on a closed stdout.
#[cfg(not(test))]
macro_rules! println {
    () => {
        $crate::utils::console::out_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::utils::console::out_line(format_args!($($arg)*))
    };
}
/// `print!` that never panics on a closed stdout.
#[cfg(not(test))]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::utils::console::out(format_args!($($arg)*))
    };
}
/// `eprintln!` that never panics on a closed stderr.
#[cfg(not(test))]
macro_rules! eprintln {
    () => {
        $crate::utils::console::err_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::utils::console::err_line(format_args!($($arg)*))
    };
}
/// `eprint!` that never panics on a closed stderr.
#[cfg(not(test))]
#[allow(unused_macros, reason = "kept so a future eprint! cannot panic either")]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::utils::console::err(format_args!($($arg)*))
    };
}

/// Command-line parsing and dispatch to the subcommands.
mod application;
/// clap definitions of all subcommands and options.
mod cli;
/// Implementations of the CLI subcommands (`put`, `get`, `verify`, …).
mod commands;
/// Config directory paths and constants.
mod config;
/// Portable configuration export/import and crypt secret portability.
mod config_sync;
mod crypt;
/// `rpool doctor` checks and the diagnostics bundle.
mod doctor;
mod drive_history;
/// Reed-Solomon parity generation and reconstruction.
mod erasure;
/// The egui desktop GUI (`rpool gui`).
mod gui;
/// Local history of finished commands.
mod history;
/// Local index of archived files (`inventory.json`).
mod inventory;
/// Resume journals for interrupted uploads and restores.
mod journal;
/// Shard integrity scan, group health and repair.
mod maintenance;
/// Archive manifests: loading, validation, replication and recovery.
mod manifest;
mod migration;
/// Serializable data model (manifests, pools, reports, records).
mod models;
mod monitor;
/// Mounted virtual drive (pool-sync, cache, spool, OS frontends).
mod mount;
/// Assignment of shards to remotes (placement strategies).
mod placement;
/// Upload plans and failure-domain checks.
mod planning;
/// Pool definitions: storage, validation, capacity and browsing.
mod pool;
/// Common imports shared by most modules.
mod prelude;
/// Shared CLI output formatting.
mod presentation;
/// Progress protocol between CLI child processes and the GUI.
mod progress;
/// Provider (cloud account) operations and limits.
mod provider;
/// Per-remote default paths ("remote roots").
mod remote_root;
mod speedtest;
/// Storage backends, readers/writers and provider admin.
mod storage;
/// Small shared helpers (file I/O, hashing, durable writes, time).
mod utils;

fn main() -> anyhow::Result<()> {
    // Stops this process's shared rclone read daemons when main returns.
    let _rclone_daemons = storage::rclone::DaemonShutdownGuard;
    application::run()
}
