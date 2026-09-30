mod application;
mod cli;
mod commands;
mod config;
mod config_sync;
mod crypt;
mod doctor;
mod erasure;
mod gui;
mod history;
mod inventory;
mod journal;
mod maintenance;
mod manifest;
mod migration;
mod models;
mod mount;
mod placement;
mod planning;
mod pool;
mod prelude;
mod presentation;
mod progress;
mod provider;
mod remote_root;
mod storage;
mod utils;

fn main() -> anyhow::Result<()> {
    application::run()
}
