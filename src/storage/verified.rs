//! Objects this process has already read back in full and found intact.
//!
//! A mounted drive used to download every new shard two or three times: once as
//! the upload readback, again when `put` re-verified the finished archive, and
//! again before the pool-sync snapshot was published. Retried sync cycles, resumed
//! upload journals and incremental re-saves read unchanged objects back once more.
//!
//! Callers that only need "is this object still the one we verified?" use
//! [`StorageReader::verify_unchanged`](super::reader::StorageReader::verify_unchanged),
//! which still stats the object every time, and skips the full download only when
//! - the same rclone setup verified the same object, size and BLAKE3 in full in
//!   this process, and
//! - the stat fingerprint (size plus provider modification time or version) is
//!   exactly what it was right after that verification.
//!
//! An object without a fingerprint is never trusted from memory. The first
//! readback of a new upload, explicit `verify`/`scrub`/repair/migration and every
//! data read keep doing full verified reads. Nothing here is persisted: a new
//! mount process verifies again.
use crate::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

/// Bounds memory on long-running mounts (about 300 bytes per entry).
const CAPACITY: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Identity of one full verified read: setup, object and expected content.
struct Key {
    /// Which rclone binary/config resolved the address.
    route: String,
    /// Raw object address.
    object: String,
    /// Expected size in bytes.
    size: u64,
    /// Expected BLAKE3 hex of the object's content.
    blake3: String,
}

/// What the provider reported for the object; must match exactly to reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    /// Object size the provider reported.
    pub(crate) size: u64,
    /// Provider modification time (opaque text), if reported.
    pub(crate) modified: Option<String>,
    /// Provider version/ETag, if reported.
    pub(crate) version: Option<String>,
}
impl Fingerprint {
    /// Size alone cannot tell a replaced object from the verified one.
    fn trustworthy(&self) -> bool {
        self.modified.as_deref().is_some_and(|m| !m.is_empty())
            || self.version.as_deref().is_some_and(|v| !v.is_empty())
    }
}

#[derive(Default)]
/// In-memory proofs of full verified reads, bounded by [`CAPACITY`] (cleared when full).
pub(crate) struct VerifiedSet {
    /// Verified identity -> fingerprint seen right after that verification.
    entries: HashMap<Key, Fingerprint>,
}
impl VerifiedSet {
    /// The lookup key of `shard` under `route`.
    fn key(route: &str, shard: &Shard) -> Key {
        Key {
            route: route.to_owned(),
            object: shard.object.clone(),
            size: shard.size,
            blake3: shard.blake3.clone(),
        }
    }
    /// True only when `now` is the very object a full read verified.
    pub(crate) fn unchanged(&self, route: &str, shard: &Shard, now: &Fingerprint) -> bool {
        now.size == shard.size
            && now.trustworthy()
            && self.entries.get(&Self::key(route, shard)) == Some(now)
    }
    /// Records a successful full verified read whose object stat was `seen`.
    pub(crate) fn record(&mut self, route: &str, shard: &Shard, seen: Fingerprint) {
        if seen.size != shard.size || !seen.trustworthy() {
            self.forget(route, &shard.object);
            return;
        }
        if self.entries.len() >= CAPACITY {
            self.entries.clear();
        }
        self.entries.insert(Self::key(route, shard), seen);
    }
    /// Drops every record of `object` (written, deleted or failed verification).
    pub(crate) fn forget(&mut self, route: &str, object: &str) {
        self.entries
            .retain(|key, _| !(key.route == route && key.object == object));
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The process-wide set behind the free functions below.
fn global() -> &'static Mutex<VerifiedSet> {
    static SET: std::sync::OnceLock<Mutex<VerifiedSet>> = std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}
/// Process-wide [`VerifiedSet::unchanged`]; false if the lock is poisoned.
/// Used by `StorageReader::verify_unchanged`.
pub(crate) fn unchanged(route: &str, shard: &Shard, now: &Fingerprint) -> bool {
    global()
        .lock()
        .map(|set| set.unchanged(route, shard, now))
        .unwrap_or(false)
}
/// Process-wide [`VerifiedSet::record`]; used by `StorageReader` after full or hash-proven verification.
pub(crate) fn record(route: &str, shard: &Shard, seen: Fingerprint) {
    if let Ok(mut set) = global().lock() {
        set.record(route, shard, seen);
    }
}
/// Process-wide [`VerifiedSet::forget`]; used by `StorageReader` before rewrites and after failed checks.
pub(crate) fn forget(route: &str, object: &str) {
    if let Ok(mut set) = global().lock() {
        set.forget(route, object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(object: &str, size: u64, hash: &str) -> Shard {
        super::super::writer::descriptor(object, size, hash.into())
    }
    fn seen(size: u64, modified: &str) -> Fingerprint {
        Fingerprint {
            size,
            modified: Some(modified.into()),
            version: None,
        }
    }

    #[test]
    fn unverified_objects_are_never_trusted() {
        let set = VerifiedSet::default();
        assert!(!set.unchanged("r", &shard("c1:a", 4, "h"), &seen(4, "t1")));
    }

    #[test]
    fn verified_unchanged_object_is_trusted() {
        let mut set = VerifiedSet::default();
        let s = shard("c1:a", 4, "h");
        set.record("r", &s, seen(4, "t1"));
        assert!(set.unchanged("r", &s, &seen(4, "t1")));
    }

    #[test]
    fn any_change_or_other_identity_requires_a_full_read() {
        let mut set = VerifiedSet::default();
        let s = shard("c1:a", 4, "h");
        set.record("r", &s, seen(4, "t1"));
        // The object changed on the provider.
        assert!(!set.unchanged("r", &s, &seen(4, "t2")));
        assert!(!set.unchanged("r", &s, &seen(5, "t1")));
        // A different expected content, object, or rclone setup.
        assert!(!set.unchanged("r", &shard("c1:a", 4, "other"), &seen(4, "t1")));
        assert!(!set.unchanged("r", &shard("c1:a", 5, "h"), &seen(5, "t1")));
        assert!(!set.unchanged("r", &shard("c1:b", 4, "h"), &seen(4, "t1")));
        assert!(!set.unchanged("other", &s, &seen(4, "t1")));
    }

    #[test]
    fn objects_without_a_fingerprint_are_not_remembered() {
        let mut set = VerifiedSet::default();
        let s = shard("c1:a", 4, "h");
        let bare = Fingerprint {
            size: 4,
            modified: None,
            version: Some(String::new()),
        };
        set.record("r", &s, bare.clone());
        assert_eq!(set.len(), 0);
        assert!(!set.unchanged("r", &s, &bare));
        let versioned = Fingerprint {
            size: 4,
            modified: None,
            version: Some("v1".into()),
        };
        set.record("r", &s, versioned.clone());
        assert!(set.unchanged("r", &s, &versioned));
    }

    #[test]
    fn forget_and_untrustworthy_records_drop_earlier_proof() {
        let mut set = VerifiedSet::default();
        let s = shard("c1:a", 4, "h");
        set.record("r", &s, seen(4, "t1"));
        set.forget("r", "c1:a");
        assert!(!set.unchanged("r", &s, &seen(4, "t1")));
        set.record("r", &s, seen(4, "t1"));
        set.record("r", &s, seen(3, "t1"));
        assert!(!set.unchanged("r", &s, &seen(4, "t1")));
    }

    #[test]
    fn capacity_is_bounded() {
        let mut set = VerifiedSet::default();
        for i in 0..CAPACITY + 5 {
            set.record("r", &shard(&format!("c1:{i}"), 1, "h"), seen(1, "t"));
        }
        assert!(set.len() <= CAPACITY);
    }
}
