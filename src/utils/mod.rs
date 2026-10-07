//! Small shared helpers used across the crate: durable file writes, positional
//! file I/O, hashing, JSON files, local time offset, remote paths, wildcard
//! matching, time and argument validation. Callers use the re-exports below.
/// Long-lived rclone children end with RPool (Windows job object).
mod child_lifetime;
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
/// Workspace directory renames that wait out briefly held files (Windows).
mod rename_dir;
/// Current unix time.
mod time;
/// Argument checks for CLI options.
mod validation;

pub(crate) use child_lifetime::tie_to_this_process;
#[allow(unused_imports, reason = "some helpers serve only one OS")]
pub(crate) use durable::{open_for_sync, persist_replacing, sync_file};
pub(crate) use file_io::{read_exact_at, write_all_at};
pub(crate) use hash::hash_file_range;
pub(crate) use json::{json_u64, prune_unknown_keys, read_json, save_json_atomic};
pub(crate) use local_offset::{local_offset_seconds, offset_label};
pub(crate) use path::{append_suffix, make_archive_id, relative_remote_object, remote_join};
pub(crate) use pattern::wildcard_match;
pub(crate) use rename_dir::rename_dir;
pub(crate) use time::now_unix;
pub(crate) use validation::ensure_positive;
