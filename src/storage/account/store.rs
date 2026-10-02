//! Loading and saving `account_limits.json` in the app config directory.
use super::limits::LimitsStore;
use crate::prelude::*;
use crate::utils::{read_json, save_json_atomic};

/// The limits at `path`; defaults when the file does not exist.
pub(crate) fn load_from(path: &Path) -> Result<LimitsStore> {
    if !path.exists() {
        return Ok(LimitsStore::default());
    }
    let store: LimitsStore = read_json(path).context("invalid account limits file")?;
    store.validate()?;
    Ok(store)
}

/// Validates `store` and writes it atomically to `path`.
pub(crate) fn save_to(path: &Path, store: &LimitsStore) -> Result<()> {
    store.validate()?;
    save_json_atomic(path, store)
}

/// Loads `account_limits.json` from the app config directory.
/// Used by `runtime::settings`, the CLI/GUI editors and config sync.
pub(crate) fn load_limits() -> Result<LimitsStore> {
    load_from(&crate::config::account_limits_path()?)
}

/// Validates and saves `store` to the app config directory; returns the path written.
pub(crate) fn save_limits(store: &LimitsStore) -> Result<PathBuf> {
    let path = crate::config::account_limits_path()?;
    save_to(&path, store)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default_and_invalid_saves_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account_limits.json");
        assert_eq!(load_from(&path).unwrap(), LimitsStore::default());
        let mut store = LimitsStore {
            bandwidth: Some("10M:off".into()),
            ..Default::default()
        };
        save_to(&path, &store).unwrap();
        assert_eq!(load_from(&path).unwrap(), store);
        store.bandwidth = Some("nonsense".into());
        assert!(save_to(&path, &store).is_err());
        assert_eq!(
            load_from(&path).unwrap().bandwidth.as_deref(),
            Some("10M:off")
        );
    }
}
