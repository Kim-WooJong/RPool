//! Small shared helpers used across the crate: durable file writes, positional
//! file I/O, hashing, JSON files, local time offset, remote paths, wildcard
//! matching, time and argument validation. Callers use the re-exports below.
mod durable;
/// Positional read/write that leaves the file cursor alone.
mod file_io;
/// BLAKE3 of file ranges.
mod hash;
/// JSON read/atomic save/pruning helpers.
mod json;
mod local_offset;
/// Archive ids and remote path joining/splitting.
mod path;
/// Case-insensitive `*`/`?` wildcard matching.
mod pattern;
/// Current unix time.
mod time;
/// Argument checks for CLI options.
mod validation;

#[allow(unused_imports, reason = "some helpers serve only one OS")]
pub(crate) use durable::{open_for_sync, persist_replacing, sync_file};
pub(crate) use file_io::{read_exact_at, write_all_at};
pub(crate) use hash::hash_file_range;
pub(crate) use json::{json_u64, prune_unknown_keys, read_json, save_json_atomic};
pub(crate) use local_offset::{local_offset_seconds, offset_label};
pub(crate) use path::{append_suffix, make_archive_id, relative_remote_object, remote_join};
pub(crate) use pattern::wildcard_match;
pub(crate) use time::now_unix;
pub(crate) use validation::ensure_positive;
