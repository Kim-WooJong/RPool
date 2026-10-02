//! Sequential-access detection and the readahead window. A read that starts
//! exactly where the previous read of the same content ended is sequential;
//! the shards after it are then fetched ahead, in parallel. Data shards of one
//! file are placed on different accounts, so a window naturally spreads the
//! downloads across remotes.
use crate::prelude::*;
use std::collections::VecDeque;

/// Remembered read streams; the oldest stream is forgotten first.
const STREAMS: usize = 64;

/// Recently read streams for sequential-access detection (most recent first).
#[derive(Default)]
pub(super) struct Sequential {
    /// (stream key, end offset of its last read), at most `STREAMS` entries.
    streams: VecDeque<(String, u64)>,
}

/// Content identity of a manifest for stream tracking.
pub(super) fn stream_key(m: &Manifest) -> String {
    format!("{}:{}", m.archive_id, m.content_root_blake3)
}

impl Sequential {
    /// Record a read of `[offset, end)`; true when it continues a stream.
    pub(super) fn observe(&mut self, key: &str, offset: u64, end: u64) -> bool {
        let position = self.streams.iter().position(|(k, _)| k == key);
        let sequential = position.is_some_and(|i| self.streams[i].1 == offset);
        if let Some(i) = position {
            self.streams.remove(i);
        }
        self.streams.push_front((key.to_owned(), end));
        self.streams.truncate(STREAMS);
        sequential
    }
}

/// Data shards starting at or after `end`, in file order, bounded by a shard
/// count and a byte budget. The first shard is always admitted when it alone
/// fits the budget.
pub(super) fn window(m: &Manifest, end: u64, shards: usize, bytes: u64) -> Vec<Shard> {
    let mut planned = vec![];
    let mut sum = 0u64;
    for s in crate::manifest::data_shards(m) {
        if s.offset < end {
            continue;
        }
        if planned.len() >= shards {
            break;
        }
        match sum.checked_add(s.size) {
            Some(next) if next <= bytes => sum = next,
            _ => break,
        }
        planned.push(s.clone());
    }
    planned
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_continuing_reads_are_sequential() {
        let mut seq = Sequential::default();
        assert!(!seq.observe("a", 0, 10));
        assert!(seq.observe("a", 10, 20));
        assert!(!seq.observe("b", 20, 30));
        assert!(!seq.observe("a", 5, 6));
        assert!(seq.observe("a", 6, 7));
        for n in 0..STREAMS {
            seq.observe(&n.to_string(), 0, 1);
        }
        assert!(!seq.observe("a", 7, 8));
    }
}
