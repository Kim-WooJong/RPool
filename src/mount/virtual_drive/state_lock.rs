//! The drive's namespace behind a mutex that notices every mutable access.
//!
//! `StateGuard` dereferences to the `Namespace` like the plain `MutexGuard`
//! it replaces. Each `DerefMut` (a field write, `save`, `*state = next`, a
//! test reaching into `events`) advances `revision`, so the visible-namespace
//! cache (`visible`) can never serve a projection of an older namespace:
//! a changed namespace is always reprojected or proven unchanged first.
use super::visible::{Pending, Projection};
use super::Namespace;
use crate::prelude::*;
use std::sync::{LockResult, MutexGuard, PoisonError};

/// `VirtualDrive::state`: the namespace mutex plus the visible-view cache
/// keyed by a mutation counter.
pub(crate) struct StateLock(Mutex<Slot>);

/// Contents of the mutex.
struct Slot {
    /// The drive's namespace.
    namespace: Namespace,
    /// Advanced on every mutable access to `namespace`.
    revision: u64,
    /// The last committed-file projection; valid while the namespace has the
    /// same event set and version (`Projection::matches`).
    projection: Option<Arc<Projection>>,
    /// The overlay built at a revision, with the projection it was built on.
    current: Option<(u64, Arc<Projection>, Arc<Pending>)>,
}

/// Lock guard that bumps the revision on every mutable access.
pub(crate) struct StateGuard<'a>(MutexGuard<'a, Slot>);

impl StateLock {
    /// Wraps `namespace` with an empty view cache.
    pub(crate) fn new(namespace: Namespace) -> Self {
        Self(Mutex::new(Slot {
            namespace,
            revision: 0,
            projection: None,
            current: None,
        }))
    }
    /// Locks the namespace; a poisoned lock still yields a guard (in the error).
    pub(crate) fn lock(&self) -> LockResult<StateGuard<'_>> {
        self.0
            .lock()
            .map(StateGuard)
            .map_err(|poisoned| PoisonError::new(StateGuard(poisoned.into_inner())))
    }
}

impl std::ops::Deref for StateGuard<'_> {
    type Target = Namespace;
    fn deref(&self) -> &Namespace {
        &self.0.namespace
    }
}

impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut Namespace {
        self.0.revision = self.0.revision.wrapping_add(1);
        &mut self.0.namespace
    }
}

impl StateGuard<'_> {
    #[cfg(test)]
    pub(super) fn revision(&self) -> u64 {
        self.0.revision
    }
    /// Cached committed-file projection, if any (validity is checked by the
    /// caller with `Projection::matches`).
    pub(super) fn projection(&self) -> Option<&Arc<Projection>> {
        self.0.projection.as_ref()
    }
    /// Stores the committed-file projection for later reuse.
    pub(super) fn set_projection(&mut self, projection: Arc<Projection>) {
        self.0.projection = Some(projection);
    }
    /// The projection and overlay, if built at the current revision.
    pub(super) fn current(&self) -> Option<(Arc<Projection>, Arc<Pending>)> {
        self.0
            .current
            .as_ref()
            .filter(|(revision, _, _)| *revision == self.0.revision)
            .map(|(_, projection, pending)| (projection.clone(), pending.clone()))
    }
    /// Stores the overlay built at the current revision.
    pub(super) fn set_current(&mut self, projection: Arc<Projection>, pending: Arc<Pending>) {
        self.0.current = Some((self.0.revision, projection, pending));
    }
}
