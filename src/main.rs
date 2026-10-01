mod application;
mod cli;
mod commands;
mod config;
mod config_sync;
mod crypt;
mod doctor;
mod drive_history;
mod erasure;
mod gui;
mod history;
mod inventory;
mod journal;
mod maintenance;
mod manifest;
mod migration;
mod models;
mod monitor;
mod mount;
mod placement;
mod planning;
mod pool;
mod prelude;
mod presentation;
mod progress;
mod provider;
mod remote_root;
mod speedtest;
mod storage;
mod utils;

fn main() -> anyhow::Result<()> {
    // Stops this process's shared rclone read daemons when main returns.
    let _rclone_daemons = storage::rclone::DaemonShutdownGuard;
    application::run()
}
