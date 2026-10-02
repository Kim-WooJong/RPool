//! CLI handlers for `rpool remote-root` (per-remote default paths kept by
//! `crate::remote_root`). Each child exposes `run`.
/// `remote-root list` handler.
pub(crate) mod list;
/// `remote-root remove` handler.
pub(crate) mod remove;
/// `remote-root set` handler.
pub(crate) mod set;
