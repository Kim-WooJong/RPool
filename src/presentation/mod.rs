//! Shared output formatting for CLI commands (byte sizes, usage table).
/// Byte-size and usage-bar formatting helpers.
mod format;
/// Quota usage table printed by `status` and `usage`.
mod usage_table;

pub(crate) use format::{format_bytes, format_optional_bytes, usage_bar};
pub(crate) use usage_table::print_usage_table;
