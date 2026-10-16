//! CLI handlers for `rpool provider`. Each child exposes `run`, re-exported
//! under the subcommand name.
/// `provider drain` handler.
mod drain;
/// `provider health` handler.
mod health;
mod keepalive;
mod limits;
/// `provider storage` handler.
mod storage;

pub(crate) use drain::run as drain;
pub(crate) use health::run as health;
pub(crate) use keepalive::run as keepalive;
pub(crate) use limits::run as limits;
pub(crate) use storage::run as storage;
