//! The Library's machine-local memory of a pool's drive metadata (one file
//! per pool under [`crate::config::library_cache_dir`]): the validated
//! events and checkpoint state of the generation listed last.
//!
//! Only an optimization. A file is used only for the same pool, the same
//! metadata generation and the same replica roots (the "scope"), and only
//! when every event still hashes to its id; anything else is ignored and
//! the next listing reads everything again and rewrites the file.
use crate::mount::metadata_browse::{generation_roots, Snapshot};
use crate::prelude::*;

const FORMAT: u32 = 1;

#[derive(Deserialize)]
struct Stored {
    format: u32,
    pool: String,
    scope: String,
    epoch: Option<String>,
    saved_unix: u64,
    snapshot: Snapshot,
}
#[derive(Serialize)]
struct Storing<'a> {
    format: u32,
    pool: &'a str,
    scope: String,
    epoch: Option<&'a str>,
    saved_unix: u64,
    snapshot: &'a Snapshot,
}

/// A usable cache file.
pub(crate) struct Cached {
    pub epoch: Option<String>,
    pub saved_unix: u64,
    pub snapshot: Snapshot,
}

fn file(dir: &Path, pool: &str) -> PathBuf {
    let name = blake3::hash(pool.as_bytes()).to_hex();
    dir.join(format!("{}.json", &name[..32]))
}

/// Identity of a generation's replicas under the current pool settings.
fn scope(pool: &str, policy: &PoolDefinition, epoch: Option<&str>) -> Result<String> {
    let roots = generation_roots(pool, policy, epoch)?;
    let key = (
        "rpool-library-cache",
        FORMAT,
        pool,
        roots,
        policy.native_crypt,
    );
    Ok(blake3::hash(&serde_json::to_vec(&key)?)
        .to_hex()
        .to_string())
}

pub(crate) fn load(pool: &str, policy: &PoolDefinition) -> Option<Cached> {
    load_in(&crate::config::library_cache_dir().ok()?, pool, policy)
}

fn load_in(dir: &Path, pool: &str, policy: &PoolDefinition) -> Option<Cached> {
    let path = file(dir, pool);
    if !path.is_file() {
        return None;
    }
    let checked = (|| -> Result<Cached> {
        let stored: Stored = crate::utils::read_json(&path)?;
        if stored.format != FORMAT || stored.pool != pool {
            bail!("written for another pool or format");
        }
        if stored.scope != scope(pool, policy, stored.epoch.as_deref())? {
            bail!("the pool's accounts changed");
        }
        Ok(Cached {
            epoch: stored.epoch,
            saved_unix: stored.saved_unix,
            snapshot: stored.snapshot.validated()?,
        })
    })();
    match checked {
        Ok(cached) => Some(cached),
        Err(error) => {
            eprintln!("Library cache of {pool} ignored ({error:#}); it is read again");
            None
        }
    }
}

/// Replaces the pool's cache file atomically.
pub(crate) fn save(
    pool: &str,
    policy: &PoolDefinition,
    epoch: Option<&str>,
    snapshot: &Snapshot,
) -> Result<()> {
    save_in(
        &crate::config::library_cache_dir()?,
        pool,
        policy,
        epoch,
        snapshot,
    )
}

fn save_in(
    dir: &Path,
    pool: &str,
    policy: &PoolDefinition,
    epoch: Option<&str>,
    snapshot: &Snapshot,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let stored = Storing {
        format: FORMAT,
        pool,
        scope: scope(pool, policy, epoch)?,
        epoch,
        saved_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        snapshot,
    };
    crate::mount::namespace::durable_json(&file(dir, pool), &stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(remotes: &[&str]) -> PoolDefinition {
        PoolDefinition {
            remotes: remotes.iter().map(|r| r.to_string()).collect(),
            ..Default::default()
        }
    }

    fn snapshot() -> Snapshot {
        let mut snapshot = Snapshot::default();
        let event: crate::mount::TestEvent = serde_json::from_value(
            serde_json::json!({"version": 1, "worker": "w", "device": "d", "path": "a.txt",
                "parents": [], "content": null}),
        )
        .unwrap();
        snapshot.events.insert(event.id().unwrap(), event);
        snapshot
    }

    #[test]
    fn cache_is_used_only_for_the_same_pool_generation_and_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let two = policy(&["a:", "b:"]);
        assert!(load_in(dir.path(), "p", &two).is_none());
        save_in(dir.path(), "p", &two, Some("e1"), &snapshot()).unwrap();
        let cached = load_in(dir.path(), "p", &two).unwrap();
        assert_eq!(cached.epoch.as_deref(), Some("e1"));
        assert_eq!(cached.snapshot.events.len(), 1);
        assert!(cached.saved_unix > 0);
        // Other accounts or another pool name: not used.
        assert!(load_in(dir.path(), "p", &policy(&["a:"])).is_none());
        assert!(load_in(dir.path(), "q", &two).is_none());
        // Tampered or corrupt files are ignored.
        let path = file(dir.path(), "p");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("a.txt", "b.txt")).unwrap();
        assert!(load_in(dir.path(), "p", &two).is_none());
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_in(dir.path(), "p", &two).is_none());
        // Saving again replaces it.
        save_in(dir.path(), "p", &two, None, &snapshot()).unwrap();
        assert_eq!(load_in(dir.path(), "p", &two).unwrap().epoch, None);
    }
}
