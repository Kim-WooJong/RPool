mod application;
mod cli;
mod commands;
mod config;
mod config_sync;
mod doctor;
mod erasure;
mod gui;
mod history;
mod inventory;
mod journal;
mod maintenance;
mod manifest;
mod models;
mod placement;
mod planning;
mod pool;
mod provider;
mod remote_root;
mod prelude;
mod presentation;
mod progress;
mod storage;
mod utils;

fn main() -> anyhow::Result<()> {
    application::run()
}
