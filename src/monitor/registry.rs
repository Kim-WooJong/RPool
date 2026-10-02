//! `<config>/mounts/<id>.json`: one [`MountEntry`] per running mount process.
use super::files::write_json_atomic;
use super::model::{MountEntry, REGISTRY_DIR};
use std::path::{Path, PathBuf};

/// `<app config dir>/mounts`, where running mounts register.
pub(crate) fn registry_dir() -> anyhow::Result<PathBuf> {
    Ok(crate::config::app_config_dir()?.join(REGISTRY_DIR))
}

/// Random 16-byte id in hex.
pub(crate) fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Still unique per process and moment; ids only name registry files.
        let seed = format!("{}-{:?}", std::process::id(), std::time::SystemTime::now());
        bytes.copy_from_slice(&blake3::hash(seed.as_bytes()).as_bytes()[..16]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Registry ids are 1-64 hex characters (they become file names).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Writes `entry` and returns its file.
pub(crate) fn register(dir: &Path, entry: &MountEntry) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_id(&entry.id), "invalid mount registry id");
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", entry.id));
    write_json_atomic(&path, entry)?;
    Ok(path)
}

/// Whether `pid` may still run. An unprovable answer counts as alive, so a
/// live mount is never unregistered by mistake.
pub(crate) fn pid_alive(pid: u32) -> bool {
    pid != 0 && crate::mount::adapter::process_alive(pid).unwrap_or(true)
}

/// Registered mounts whose process is alive, sorted by pool (then
/// mountpoint). Entries of dead processes are deleted; unreadable files are
/// skipped.
pub(crate) fn list(dir: &Path, alive: impl Fn(u32) -> bool) -> Vec<MountEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut mounts = Vec::new();
    for file in entries.flatten() {
        let path = file.path();
        let name = file.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let Some(entry) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<MountEntry>(&bytes).ok())
        else {
            continue;
        };
        if alive(entry.pid) {
            mounts.push(entry);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    mounts.sort_by(|a, b| (&a.pool, &a.mountpoint, &a.id).cmp(&(&b.pool, &b.mountpoint, &b.id)));
    mounts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, pool: &str, pid: u32) -> MountEntry {
        MountEntry {
            id: id.into(),
            pool: pool.into(),
            workspace: "/w".into(),
            mountpoint: format!("/mnt/{pool}"),
            frontend: "dav".into(),
            pid,
            started_unix: 1,
        }
    }

    #[test]
    fn lists_alive_sorted_and_deletes_stale_entries() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mounts");
        assert!(list(&dir, |_| true).is_empty());
        let stale = register(&dir, &entry("aa", "zeta", 7)).unwrap();
        register(&dir, &entry("bb", "beta", 8)).unwrap();
        register(&dir, &entry("cc", "alpha", 9)).unwrap();
        std::fs::write(dir.join("dd.json"), b"{bad").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let mounts = list(&dir, |pid| pid != 7);
        let pools: Vec<_> = mounts.iter().map(|m| m.pool.as_str()).collect();
        assert_eq!(pools, ["alpha", "beta"]);
        assert!(!stale.exists());
        assert!(dir.join("dd.json").exists() && dir.join("notes.txt").exists());
        assert!(register(&dir, &entry("../x", "p", 1)).is_err());
    }

    #[test]
    fn ids_are_random_hex_and_own_pid_is_alive() {
        let (a, b) = (new_id(), new_id());
        assert!(valid_id(&a) && a.len() == 32 && a != b);
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
    }
}
