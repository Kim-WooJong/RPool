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
use super::core::lock;
use super::error::FsResult;
use super::generation::Generation;
use crate::prelude::*;
use std::ops::{Deref, DerefMut};
use std::sync::MutexGuard;

/// `(size, intent id)` of a file's unsealed generation.
pub(super) type Summary = Option<(u64, String)>;

#[derive(Default)]
/// Per-file slot: start lock, current unsealed generation and its lock-free summary.
pub(super) struct SlotCell {
    /// Serializes generation starts (and so baseline copies) of this file.
    start: Mutex<()>,
    /// Unsealed generation, `None` when the file has no unsealed writes.
    generation: Mutex<Option<Generation>>,
    /// Published [`Summary`] of `generation`, refreshed when a guard drops.
    summary: Mutex<Summary>,
}
/// Shared handle to a file's slot (kept in `FsCore::slots`).
pub(super) type Slot = Arc<SlotCell>;

impl SlotCell {
    /// Held across a generation start; take it before the namespace lock.
    pub(super) fn starting(&self) -> FsResult<MutexGuard<'_, ()>> {
        lock(&self.start)
    }
    /// Locks the generation; the returned guard republishes the summary on drop.
    pub(super) fn lock(&self) -> FsResult<SlotGuard<'_>> {
        Ok(SlotGuard {
            generation: lock(&self.generation)?,
            summary: &self.summary,
        })
    }
    /// The generation as of its last guard release. Never waits for a
    /// generation lock, so it never waits for another file's I/O.
    pub(super) fn summary(&self) -> FsResult<Summary> {
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
    summary: &'a Mutex<Summary>,
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
        if *summary != current {
            *summary = current;
        }
    }
}
