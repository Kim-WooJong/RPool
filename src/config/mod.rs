//! Application configuration: built-in constants and the locations of RPool's
//! settings files in the per-user config directory.
/// Defaults and limits (shard size, workers, coding).
pub(crate) mod constants;
/// Settings file paths under the config directory.
mod paths;

pub(crate) use paths::{
    account_limits_path, account_usage_path, app_config_dir, gui_settings_path, history_path,
    integrity_snapshot_path, inventory_path, library_cache_dir, pools_path, remote_roots_path,
};
