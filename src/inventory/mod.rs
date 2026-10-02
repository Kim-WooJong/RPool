//! Local inventory: an index (`inventory.json` in the config directory) of
//! archived files keyed by archive id, with where each manifest lives. Used by
//! the `inventory` commands, uploads, migrations, the GUI and `doctor`.

/// Adding an archive from its manifest.
mod add;
/// Converting a manifest into an entry.
mod entry;
/// Loading the index.
mod load;
/// Searching entries by pattern.
mod query;
/// Rebuilding the index from manifest files on disk.
mod rebuild;
/// Removing an archive's entry.
mod remove;
/// Saving the index atomically.
mod save;

pub(crate) use add::add_manifest;
pub(crate) use entry::entry_from_manifest;
pub(crate) use load::load_inventory;
pub(crate) use query::find_entries;
pub(crate) use rebuild::rebuild_from_directory;
pub(crate) use remove::remove_entry;
pub(crate) use save::save_inventory;
