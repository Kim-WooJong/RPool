//! Ranged reads inside a shard (seeks). A full shard download always starts
//! at the shard's first byte, so a reader that jumps into the middle of a
//! missing 64 MiB shard would wait until the download reaches it. Instead the
//! needed part is read directly with a ranged request, in aligned chunks kept
//! in a small in-memory window cache, and the next chunks of that position are
//! read ahead in the background. The full download keeps running and is the
//! only way bytes enter the (verified) disk cache.
//!
//! Integrity: chunk bytes are not BLAKE3-verified (they are a part of the
//! shard). Pool remotes are rclone crypt remotes, which authenticate every
//! 64 KiB block with the key, so the trade-off matches prefix serving (see
//! `ShardCache`). Chunks never reach the disk cache, and a later full
//! download that fails verification is logged like a served prefix.
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use std::collections::{HashMap, VecDeque};
use std::sync::Condvar;

/// Chunk size of ranged reads (aligned within the shard).
pub(super) const CHUNK: u64 = 4 << 20;
/// Chunks kept in memory across all shards (oldest dropped first).
const CHUNKS_KEPT: usize = 24;
/// Chunks read ahead of the one just served, in the background.
const AHEAD: u64 = 2;
/// A read starting at most this far past the full download's progress waits
/// for it instead (a sequential reader that merely caught up).
pub(super) const NEAR: u64 = 1 << 20;

/// One chunk: loading or loaded (a failed read removes its slot).
enum Slot {
    /// A thread is reading it.
    Loading,
    /// Its bytes.
    Ready(Arc<Vec<u8>>),
}

/// Chunk table guarded by `Windows::state`.
#[derive(Default)]
struct State {
    /// (entry name, chunk start within the shard) -> slot.
    slots: HashMap<(String, u64), Slot>,
    /// Ready chunks, oldest first, for the memory bound.
    order: VecDeque<(String, u64)>,
}

/// In-memory chunk cache of ranged reads, shared by background threads.
#[derive(Default)]
pub(super) struct Windows {
    /// Chunk table.
    state: Mutex<State>,
    /// Signalled when a chunk finishes loading.
    loaded: Condvar,
}

impl Windows {
    /// Locks the table, recovering from poisoning (plain map updates).
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends `[within, within + n)` of shard `s` (cache entry `name`)
    /// to `out` from ranged chunks, reading missing ones now and the next
    /// `AHEAD` in the background. `Ok(false)`: a chunk could not be read and
    /// nothing was appended; the caller waits for the full download instead.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn read(
        self: &Arc<Self>,
        reader: &Arc<StorageReader>,
        name: &str,
        s: &Shard,
        within: u64,
        n: usize,
        out: &mut Vec<u8>,
        retries: u32,
    ) -> Result<bool> {
        let end = within + n as u64;
        let first = within / CHUNK * CHUNK;
        let mut parts = vec![];
        let mut start = first;
        while start < end {
            match self.chunk(reader, name, s, start, retries) {
                Some(bytes) => parts.push((start, bytes)),
                None => return Ok(false),
            }
            start += CHUNK;
        }
        for (chunk_start, bytes) in parts {
            let from = within.max(chunk_start) - chunk_start;
            let to = (end.min(chunk_start + bytes.len() as u64)) - chunk_start;
            out.extend_from_slice(&bytes[from as usize..to as usize]);
        }
        for k in 1..=AHEAD {
            let next = start - CHUNK + k * CHUNK;
            if next < s.size {
                self.spawn(reader, name, s, next, retries);
            }
        }
        Ok(true)
    }

    /// Drops every chunk of `name` (its verified entry now exists).
    pub(super) fn forget(&self, name: &str) {
        let mut st = self.state();
        st.slots
            .retain(|(n, _), slot| n != name || matches!(slot, Slot::Loading));
        st.order.retain(|(n, _)| n != name);
    }

    /// The chunk at `start`: waits for one loading, else reads it on this
    /// thread. `None` when the read failed.
    fn chunk(
        &self,
        reader: &StorageReader,
        name: &str,
        s: &Shard,
        start: u64,
        retries: u32,
    ) -> Option<Arc<Vec<u8>>> {
        let key = (name.to_owned(), start);
        let mut st = self.state();
        loop {
            match st.slots.get(&key) {
                Some(Slot::Ready(bytes)) => {
                    let bytes = Arc::clone(bytes);
                    st.order.retain(|k| k != &key);
                    st.order.push_back(key);
                    return Some(bytes);
                }
                Some(Slot::Loading) => {
                    st = self.loaded.wait(st).unwrap_or_else(|e| e.into_inner());
                }
                None => break,
            }
        }
        st.slots.insert(key.clone(), Slot::Loading);
        drop(st);
        Some(self.load(reader, key, s, retries)).flatten()
    }

    /// Reads chunk `key` in the background unless present or loading.
    fn spawn(
        self: &Arc<Self>,
        reader: &Arc<StorageReader>,
        name: &str,
        s: &Shard,
        start: u64,
        retries: u32,
    ) {
        let key = (name.to_owned(), start);
        {
            let mut st = self.state();
            if matches!(st.slots.get(&key), Some(Slot::Ready(_) | Slot::Loading)) {
                return;
            }
            st.slots.insert(key.clone(), Slot::Loading);
        }
        let (windows, reader, shard) = (Arc::clone(self), Arc::clone(reader), s.clone());
        let failed_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("rpool-range-ahead".into())
            .spawn(move || {
                let _ = windows.load(&reader, key, &shard, retries);
            });
        if spawned.is_err() {
            let mut st = self.state();
            st.slots.remove(&failed_key);
            drop(st);
            self.loaded.notify_all();
        }
    }

    /// Reads chunk `key` (marked loading by the caller) and publishes it.
    fn load(
        &self,
        reader: &StorageReader,
        key: (String, u64),
        s: &Shard,
        retries: u32,
    ) -> Option<Arc<Vec<u8>>> {
        let start = key.1;
        let length = CHUNK.min(s.size - start);
        let result = reader.read_range(s, start, length, retries);
        let mut st = self.state();
        let bytes = match result {
            Ok(bytes) => {
                let bytes = Arc::new(bytes);
                st.slots
                    .insert(key.clone(), Slot::Ready(Arc::clone(&bytes)));
                st.order.push_back(key);
                while st.order.len() > CHUNKS_KEPT {
                    if let Some(old) = st.order.pop_front() {
                        st.slots.remove(&old);
                    }
                }
                Some(bytes)
            }
            Err(_) => {
                // Waiters retry the read themselves or fall back to the full download.
                st.slots.remove(&key);
                None
            }
        };
        drop(st);
        self.loaded.notify_all();
        bytes
    }

    /// Chunks held in memory (tests).
    #[cfg(test)]
    pub(super) fn ready_chunks(&self) -> usize {
        self.state().order.len()
    }
}
