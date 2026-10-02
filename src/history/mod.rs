//! Local task history: one JSON line per finished CLI command (operation,
//! target, timing, status, redacted error). Written by `application` around
//! each command; read by `rpool history`, the GUI dashboard and `doctor`.

/// Appending a record.
mod append;
/// Loading all records.
mod load;
/// Keeping only the newest records.
mod prune;
/// Redacting secrets from recorded text.
mod redact;
/// Turning a command into a pending record and finishing it.
mod track;

pub(crate) use append::append_record;
pub(crate) use load::load_history;
pub(crate) use prune::prune_history;
pub(crate) use redact::redact_text;
pub(crate) use track::{describe_command, finish_record};
