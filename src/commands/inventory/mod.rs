//! CLI handlers for `rpool inventory` (the rebuildable local archive index in
//! `crate::inventory`). Each child exposes `run`, re-exported under the subcommand name.
/// `inventory add` handler.
mod add;
/// `inventory find` handler.
mod find;
/// `inventory info` handler.
mod info;
/// `inventory list` handler.
mod list;
/// `inventory rebuild` handler.
mod rebuild;

pub(crate) use add::run as add;
pub(crate) use find::run as find;
pub(crate) use info::run as info;
pub(crate) use list::run as list;
pub(crate) use rebuild::run as rebuild;
