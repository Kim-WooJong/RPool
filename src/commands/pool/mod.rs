//! CLI handlers for `rpool pool`. Each child exposes `run`, re-exported under
//! the subcommand name; `migrate_history` labels migrations for `rpool history`.
/// `pool browse`: read-only listing of a pool's drive from cloud metadata.
mod browse;
mod compact;
/// `pool list` handler.
mod list;
mod migrate;
mod migrate_retire;
/// `pool remove` handler.
mod remove;
/// `pool set` handler.
mod set;
/// `pool show` handler.
mod show;

pub(crate) use browse::run as browse;
pub(crate) use compact::run as compact;
pub(crate) use list::run as list;
pub(crate) use migrate::{history_label as migrate_history, run as migrate};
pub(crate) use remove::run as remove;
pub(crate) use set::run as set;
pub(crate) use show::run as show;
