//! In-memory LRU index of published clean cache entries. Built by one scan at
//! open, then kept current by publish/evict, so admission never rescans the
//! directory. Only names of the form `<blake3>-<size>` are entries; unknown
//! files (dirty data, foreign files) are never counted or removed.
use crate::prelude::*;
use std::collections::HashMap;
use std::time::Duration;

/// Who asks for space. A demand read may evict any unpinned entry; readahead
/// may only evict entries idle for at least [`PREFETCH_PROTECT`], so it never
/// displaces data the user is actively reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Admission {
    Demand,
    Prefetch,
}

/// Recently accessed entries readahead must not evict.
pub(super) const PREFETCH_PROTECT: Duration = Duration::from_secs(30);

struct Entry {
    size: u64,
    accessed: SystemTime,
    /// Open reads copying from this entry; a pinned entry is never evicted.
    pins: u32,
}

#[derive(Default)]
pub(super) struct Index {
    entries: HashMap<String, Entry>,
    /// Sum of published entry sizes.
    total: u64,
    /// Bytes promised to in-flight downloads and restores.
    reserved: u64,
}

pub(super) fn is_entry_name(name: &str) -> bool {
    let Some((hash, size)) = name.split_once('-') else {
        return false;
    };
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && size.parse::<u64>().is_ok()
}

impl Index {
    pub(super) fn scan(root: &Path) -> Result<Self> {
        let mut index = Self::default();
        for item in fs::read_dir(root)? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            if !is_entry_name(&name) {
                continue;
            }
            let m = fs::symlink_metadata(item.path())?;
            if m.is_file() && !m.file_type().is_symlink() {
                index.insert(name, m.len(), m.accessed()?);
            }
        }
        Ok(index)
    }
    pub(super) fn used(&self) -> u64 {
        self.total.saturating_add(self.reserved)
    }
    pub(super) fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }
    /// Publish or replace an entry (an atomic rename over the same name).
    pub(super) fn insert(&mut self, name: String, size: u64, accessed: SystemTime) {
        let pins = match self.entries.remove(&name) {
            Some(old) => {
                self.total -= old.size;
                old.pins
            }
            None => 0,
        };
        self.total += size;
        self.entries.insert(
            name,
            Entry {
                size,
                accessed,
                pins,
            },
        );
    }
    /// Forget an entry whose file is gone or invalid. Returns its size.
    pub(super) fn forget(&mut self, name: &str) -> Option<u64> {
        let entry = self.entries.remove(name)?;
        self.total -= entry.size;
        Some(entry.size)
    }
    pub(super) fn pin(&mut self, name: &str) -> bool {
        match self.entries.get_mut(name) {
            Some(entry) => {
                entry.pins += 1;
                true
            }
            None => false,
        }
    }
    pub(super) fn unpin(&mut self, name: &str, accessed: Option<SystemTime>) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.pins = entry.pins.saturating_sub(1);
            if let Some(time) = accessed {
                entry.accessed = entry.accessed.max(time);
            }
        }
    }
    pub(super) fn reserve(&mut self, bytes: u64) {
        self.reserved += bytes;
    }
    pub(super) fn release(&mut self, bytes: u64) {
        self.reserved = self.reserved.saturating_sub(bytes);
    }
    /// Oldest-accessed entry this admission may evict, if any.
    fn victim(&self, mode: Admission, now: SystemTime) -> Option<String> {
        self.entries
            .iter()
            .filter(|(_, e)| e.pins == 0)
            .filter(|(_, e)| {
                mode == Admission::Demand
                    || now
                        .duration_since(e.accessed)
                        .is_ok_and(|idle| idle >= PREFETCH_PROTECT)
            })
            .min_by(|a, b| a.1.accessed.cmp(&b.1.accessed).then(a.0.cmp(b.0)))
            .map(|(name, _)| name.clone())
    }
    /// Delete one evictable entry. Returns the bytes freed, or `None` when no
    /// entry may be evicted.
    pub(super) fn evict_one(
        &mut self,
        root: &Path,
        mode: Admission,
        now: SystemTime,
    ) -> Result<Option<(PathBuf, u64)>> {
        let Some(name) = self.victim(mode, now) else {
            return Ok(None);
        };
        let path = root.join(&name);
        match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let size = self.forget(&name).unwrap_or(0);
        Ok(Some((path, size)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn victims_are_oldest_unpinned_and_prefetch_spares_recent_entries() {
        let temp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let old = now - Duration::from_secs(3600);
        let mut index = Index::default();
        for (n, t) in [("a", old), ("b", old + Duration::from_secs(1)), ("c", now)] {
            fs::write(temp.path().join(n), b"xx").unwrap();
            index.insert(n.into(), 2, t);
        }
        assert!(index.pin("a"));
        let (path, size) = index
            .evict_one(temp.path(), Admission::Prefetch, now)
            .unwrap()
            .unwrap();
        assert_eq!((path, size), (temp.path().join("b"), 2));
        // Only the pinned old entry and the recent entry remain.
        assert!(index
            .evict_one(temp.path(), Admission::Prefetch, now)
            .unwrap()
            .is_none());
        let (path, _) = index
            .evict_one(temp.path(), Admission::Demand, now)
            .unwrap()
            .unwrap();
        assert_eq!(path, temp.path().join("c"));
        assert!(index
            .evict_one(temp.path(), Admission::Demand, now)
            .unwrap()
            .is_none());
        index.unpin("a", Some(now));
        assert_eq!(index.used(), 2);
        assert!(temp.path().join("a").exists());
    }
}
