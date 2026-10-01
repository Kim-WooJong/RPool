//! Parent folders are created once, one at a time per remote, before an
//! upload or copy. rclone creates missing parents itself on every write, but
//! parallel writes into a new folder then race to create it, and some servers
//! reject the loser (SFTP on Windows: `mkParentDir failed`; WebDAV: `423
//! Locked`). Creating the folder first, serialized per remote, removes the
//! race. Best effort: when `rclone mkdir` fails the write goes ahead as
//! before, so this can only remove failures, never add one.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Folders created (or found) by this process, per rclone instance.
fn known() -> &'static Mutex<HashSet<String>> {
    static KNOWN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// One lock per remote: folder creation on a remote is serialized.
fn remote_lock(key: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(key.to_owned()).or_default().clone()
}

/// `remote:a/b/object` -> `remote:a/b`; `None` for an object directly at a
/// remote's root (nothing to create).
pub(super) fn parent(address: &str) -> Option<&str> {
    let colon = address.find(':')?;
    let (_, path) = address.split_at(colon + 1);
    let path = path.trim_end_matches('/');
    let slash = path.rfind('/')?;
    let parent = &address[..colon + 1 + slash];
    // `remote:/object` has the absolute root as its parent.
    (!parent.ends_with(':') && !parent.ends_with(":/")).then_some(parent)
}

/// Runs `create(parent)` at most once per `(instance, parent)` for which it
/// succeeded; callers for the same remote wait for each other.
pub(super) fn ensure_parent(instance: &str, address: &str, create: impl FnOnce(&str) -> bool) {
    let Some(parent) = parent(address) else {
        return;
    };
    let key = format!("{instance}\u{0}{parent}");
    let seen = |key: &str| {
        known()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
    };
    if seen(&key) {
        return;
    }
    let remote = &parent[..parent.find(':').unwrap_or(0)];
    let lock = remote_lock(&format!("{instance}\u{0}{remote}"));
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if seen(&key) {
        return; // Created while we waited.
    }
    if create(parent) {
        known()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn parent_of_addresses() {
        assert_eq!(parent("r:a/b/c.bin"), Some("r:a/b"));
        assert_eq!(parent("r:a/c.bin"), Some("r:a"));
        assert_eq!(parent("r:c.bin"), None);
        assert_eq!(parent("r:/c.bin"), None);
        assert_eq!(parent("r:/x/c.bin"), Some("r:/x"));
        assert_eq!(parent("no-colon"), None);
    }

    #[test]
    fn parallel_writes_create_a_folder_once_and_one_at_a_time() {
        let calls = AtomicUsize::new(0);
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for i in 0..16 {
                let (calls, running, peak) = (&calls, &running, &peak);
                scope.spawn(move || {
                    let address = format!("r:arch-{}/data/{i}.bin", i % 2);
                    ensure_parent("t-once", &address, |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        running.fetch_sub(1, Ordering::SeqCst);
                        true
                    });
                });
            }
        });
        // Two distinct folders, each created once, never concurrently.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_creation_is_tried_again_later() {
        let calls = AtomicUsize::new(0);
        ensure_parent("t-fail", "r:x/y.bin", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        });
        ensure_parent("t-fail", "r:x/z.bin", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        });
        ensure_parent("t-fail", "r:x/w.bin", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
