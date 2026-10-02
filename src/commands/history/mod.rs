//! CLI handlers for `rpool history` (operation history kept by `crate::history`).
/// `history list` handler.
mod list;
/// `history prune` handler.
mod prune;

pub(crate) use list::run as list;
pub(crate) use prune::run as prune;
