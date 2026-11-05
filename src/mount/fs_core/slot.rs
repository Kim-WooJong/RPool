//! One file's generation slot.
//!
//! Starting a generation can copy a whole baseline (possibly downloading a
//! cloud revision), so it runs holding only the slot's `start` lock: the
//! namespace and generation locks are taken just to plan the start and to
//! install the prepared generation. Lookups and listings never lock a
//! generation; they read `summary`, a leaf lock that every generation guard
//! refreshes when it is dropped, so a busy file never stalls the rest of the
//! drive.
//!
//! Lock order: `start` → namespace → `generation` → drive locks; `summary` is
//! a leaf.
//!
//! Every published summary change gets a fresh stamp, and a generation
//! appearing or disappearing advances [`Counters::presence`], both from the
//! owning core's [`Counters`], so a reader that sees the
//! same stamps and presence before and after some work knows no summary it
//! could have read changed in between, even if it changed back (a size A → B
//! → A) or a whole generation came and went.
use super::core::lock;
use super::error::FsResult;
use super::generation::Generation;
use crate::prelude::*;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::MutexGuard;

/// Summary change counters shared by all slots of one core.
#[derive(Default)]
pub(super) struct Counters {
    /// Last stamp handed out; only grows, so no stamp is published twice in
    /// a core, not even by a slot created after another was dropped.
    stamps: AtomicU64,
    /// Advanced (under the slot's summary lock) whenever one of the core's
    /// summaries goes from `None` to `Some` or back.
    presence: AtomicU64,
}
impl Counters {
    /// How often one of the core's generations appeared or disappeared.
    pub(super) fn presence(&self) -> u64 {
        self.presence.load(Ordering::SeqCst)
    }
}

/// `(size, intent id)` of a file's unsealed generation.
pub(super) type Summary = Option<(u64, String)>;

/// Per-file slot: start lock, current unsealed generation and its lock-free summary.
pub(super) struct SlotCell {
    /// Serializes generation starts (and so baseline copies) of this file.
    start: Mutex<()>,
    /// Unsealed generation, `None` when the file has no unsealed writes.
    generation: Mutex<Option<Generation>>,
    /// Published [`Summary`] of `generation`, refreshed when a guard drops,
    /// with the stamp of its last change (0: never published).
    summary: Mutex<(Summary, u64)>,
    /// The owning core's counters.
    counters: Arc<Counters>,
}
/// Shared handle to a file's slot (kept in `FsCore::slots`).
pub(super) type Slot = Arc<SlotCell>;

impl SlotCell {
    /// An empty slot publishing its changes through `counters`.
    pub(super) fn new(counters: Arc<Counters>) -> Self {
        Self {
            start: Mutex::new(()),
            generation: Mutex::new(None),
            summary: Mutex::new((None, 0)),
            counters,
        }
    }
    /// Held across a generation start; take it before the namespace lock.
    pub(super) fn starting(&self) -> FsResult<MutexGuard<'_, ()>> {
        lock(&self.start)
    }
    /// Locks the generation; the returned guard republishes the summary on drop.
    pub(super) fn lock(&self) -> FsResult<SlotGuard<'_>> {
        Ok(SlotGuard {
            generation: lock(&self.generation)?,
            summary: &self.summary,
            counters: &self.counters,
        })
    }
    /// The generation as of its last guard release. Never waits for a
    /// generation lock, so it never waits for another file's I/O.
    pub(super) fn summary(&self) -> FsResult<Summary> {
        Ok(lock(&self.summary)?.0.clone())
    }
    /// [`Self::summary`] with the stamp of its last change; equal stamps
    /// read at two times mean the summary did not change in between.
    pub(super) fn stamped(&self) -> FsResult<(Summary, u64)> {
        Ok(lock(&self.summary)?.clone())
    }
    /// No start in progress and no generation: the slot may be dropped.
    pub(super) fn idle(&self) -> bool {
        self.start.try_lock().is_ok() && self.generation.try_lock().is_ok_and(|g| g.is_none())
    }
}

/// The locked generation. Dropping it publishes the summary, so a listing
/// sees each change once it is complete, never a half-applied one.
pub(super) struct SlotGuard<'a> {
    /// Locked generation.
    generation: MutexGuard<'a, Option<Generation>>,
    /// Summary to refresh on drop.
    summary: &'a Mutex<(Summary, u64)>,
    /// Counters to advance on a change.
    counters: &'a Counters,
}
impl Deref for SlotGuard<'_> {
    type Target = Option<Generation>;
    fn deref(&self) -> &Self::Target {
        &self.generation
    }
}
impl DerefMut for SlotGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.generation
    }
}
impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let current = self
            .generation
            .as_ref()
            .map(|g| (g.size, g.intent.id.clone()));
        let mut summary = self.summary.lock().unwrap_or_else(|e| e.into_inner());
        if summary.0 != current {
            // Both counters advance before the new summary can be read.
            if summary.0.is_some() != current.is_some() {
                self.counters.presence.fetch_add(1, Ordering::SeqCst);
            }
            let stamp = self.counters.stamps.fetch_add(1, Ordering::SeqCst) + 1;
            *summary = (current, stamp);
        }
    }
}
