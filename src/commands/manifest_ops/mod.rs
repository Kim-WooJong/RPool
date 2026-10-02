//! CLI handlers for `rpool manifest` (manifest replicas stored on the providers).
//! Each child exposes `run`, re-exported under the subcommand name.
/// `manifest recover` handler.
mod recover;
/// `manifest replicate` handler.
mod replicate;
/// `manifest verify` handler.
mod verify;

pub(crate) use recover::run as recover;
pub(crate) use replicate::run as replicate;
pub(crate) use verify::run as verify;
