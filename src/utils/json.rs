//! JSON file helpers for config, state and manifest files: read, atomic save,
//! pruning of obsolete keys, and small value accessors.
use crate::prelude::*;
use crate::utils::append_suffix;

/// Reads and deserializes the JSON file at `path`.
pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Writes `value` as pretty JSON via a per-process temp file renamed into place,
/// creating the parent folder if needed (any existing file is removed first).
pub(crate) fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = append_suffix(path, &format!(".tmp-{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Rewrites the JSON file at `path` without the keys `T` no longer knows
/// (options removed by earlier versions), after copying it to `<file>.bak`.
/// Untouched when it is missing, already clean, or does not parse as `T`
/// (then the caller's own error handling applies). Returns the removed key
/// paths (`a.b`); renamed fields show under their old name.
pub(crate) fn prune_unknown_keys<T: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw: Value = serde_json::from_slice(&fs::read(path)?)?;
    let parsed: T = serde_json::from_value(raw.clone())?;
    let clean = serde_json::to_value(&parsed)?;
    let mut removed = Vec::new();
    unknown_keys(&raw, &clean, "", &mut removed);
    if !removed.is_empty() {
        fs::copy(path, append_suffix(path, ".bak"))?;
        save_json_atomic(path, &parsed)?;
    }
    Ok(removed)
}

/// Keys of `raw` that `clean` lacks, recursively (null values do not count:
/// they are what an omitted optional field means).
fn unknown_keys(raw: &Value, clean: &Value, at: &str, out: &mut Vec<String>) {
    let join = |key: &str| {
        if at.is_empty() {
            key.to_owned()
        } else {
            format!("{at}.{key}")
        }
    };
    match (raw, clean) {
        (Value::Object(raw), Value::Object(clean)) => {
            for (key, value) in raw {
                match clean.get(key) {
                    Some(kept) => unknown_keys(value, kept, &join(key), out),
                    None if value.is_null() => {}
                    None => out.push(join(key)),
                }
            }
        }
        (Value::Array(raw), Value::Array(clean)) if raw.len() == clean.len() => {
            for (i, (value, kept)) in raw.iter().zip(clean).enumerate() {
                unknown_keys(value, kept, &join(&i.to_string()), out);
            }
        }
        _ => {}
    }
}

/// `value[key]` as a `u64`, if present and numeric. Used by `storage::admin::quota`.
pub(crate) fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Profile {
        workspace: String,
    }
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Settings {
        workers: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        profiles: std::collections::BTreeMap<String, Profile>,
    }

    #[test]
    fn removed_options_are_dropped_with_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui-settings.json");
        let old = r#"{"workers":7,"note":null,"online_drive":true,
            "profiles":{"A":{"workspace":"/w","upload_files":8,"workers":12}}}"#;
        fs::write(&path, old).unwrap();
        let removed = prune_unknown_keys::<Settings>(&path).unwrap();
        assert_eq!(
            removed,
            [
                "online_drive",
                "profiles.A.upload_files",
                "profiles.A.workers"
            ]
        );
        let now: Value = read_json(&path).unwrap();
        assert_eq!(
            now,
            serde_json::json!({"workers":7,"profiles":{"A":{"workspace":"/w"}}})
        );
        assert_eq!(
            fs::read_to_string(append_suffix(&path, ".bak")).unwrap(),
            old
        );
        // Clean now: nothing changes, no new backup is needed.
        fs::remove_file(append_suffix(&path, ".bak")).unwrap();
        assert!(prune_unknown_keys::<Settings>(&path).unwrap().is_empty());
        assert!(!append_suffix(&path, ".bak").exists());
    }

    #[test]
    fn missing_or_unreadable_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(prune_unknown_keys::<Settings>(&path).unwrap().is_empty());
        fs::write(&path, r#"{"workers":"many","old":1}"#).unwrap();
        assert!(prune_unknown_keys::<Settings>(&path).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"workers":"many","old":1}"#
        );
    }
}
