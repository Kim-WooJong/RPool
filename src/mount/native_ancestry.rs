//! Ancestry for the native filesystem core in pool-sync workspaces. A native
//! frontend knows what an editor opened and read, so an edit descends from
//! the revision whose bytes it was built on, not from whatever is visible at
//! write time. A peer's concurrent edit therefore becomes a preserved
//! conflict instead of being overwritten.
use super::namespace::Intent;
use super::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;

/// A rename the native frontend should let the application redo as copy and
/// delete (EXDEV): v7 cannot express it while intents are pending.
#[derive(Debug)]
pub(crate) struct CrossDevice(pub(crate) &'static str);
impl std::fmt::Display for CrossDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rename not supported here yet: {}", self.0)
    }
}
impl std::error::Error for CrossDevice {}

/// An operation that must wait for the running upload of the same file.
#[derive(Debug)]
pub(crate) struct Busy(pub(crate) &'static str);
impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "busy: {}", self.0)
    }
}
impl std::error::Error for Busy {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ancestry {
    /// Continues this workspace's pending or already committed intent.
    Continue(String),
    /// Descends from these namespace events.
    Parents(Vec<String>),
    /// The drive's conservative default (last persisted read, else visible).
    Default,
}
impl Ancestry {
    /// The ancestry of an edit whose bytes start from `revision`.
    pub(crate) fn of(revision: &Revision) -> Self {
        match revision {
            Revision::Local { id, .. } => Self::Continue(id.clone()),
            Revision::Cloud { id, .. } => Self::Parents(vec![id.clone()]),
        }
    }
}

impl VirtualDrive {
    /// Protects a served revision from retention for this session. Unlike
    /// `pin_read`, it adds no DAV range fence and records no ancestry.
    pub(crate) fn observe_open(&self, path: &str, revision: &Revision) {
        if let Ok(mut pins) = self.pins.lock() {
            pins.entry(path.into()).or_insert_with(|| revision.clone());
        }
    }

    /// Persists the cloud event an editor actually read as the path's base,
    /// so an edit after a remount still descends from it.
    pub(crate) fn observe_read(&self, path: &str, event: &str) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let known = s.events.contains_key(event);
        let current = s
            .bases
            .get(path)
            .is_some_and(|b| b.len() == 1 && b[0] == event);
        if !known || current {
            return Ok(());
        }
        let mut next = s.clone();
        next.bases.insert(path.into(), vec![event.into()]);
        next.save(&self.root)?;
        *s = next;
        Ok(())
    }

    /// Ancestry for an edit that inherits no bytes and has no read revision:
    /// this workspace's latest pending intent at `path` (a save or delete not
    /// yet uploaded), else, if the path does not exist, the events that ended
    /// its history (so a recreate follows the delete), else the default.
    pub(crate) fn unread_ancestry(&self, path: &str, exists: bool) -> Result<Ancestry> {
        let s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        if let Some(latest) = s.pending.iter().rev().find(|i| i.path == path) {
            return Ok(Ancestry::Continue(latest.id.clone()));
        }
        if !exists {
            let referenced: BTreeSet<&String> =
                s.events.values().flat_map(|e| e.parents.iter()).collect();
            let heads: Vec<String> = s
                .events
                .iter()
                .filter(|(id, e)| e.path == path && !referenced.contains(id))
                .map(|(id, _)| id.clone())
                .collect();
            if !heads.is_empty() {
                return Ok(Ancestry::Parents(heads));
            }
        }
        Ok(Ancestry::Default)
    }

    pub(super) fn apply_ancestry(&self, intent: &mut Intent, ancestry: &Ancestry) -> Result<()> {
        let s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        match ancestry {
            Ancestry::Default => {}
            Ancestry::Continue(id) => {
                if let Some(previous) = s.pending.iter().find(|i| &i.id == id) {
                    intent.depends_on = Some(id.clone());
                    intent.event_path = previous.event_path.clone();
                    intent.parents.clear();
                } else if let Some(event) = s.committed_intents.get(id) {
                    intent.depends_on = None;
                    intent.parents = vec![event.clone()];
                    if let Some(parent) = s.events.get(event) {
                        intent.event_path = parent.path.clone();
                    }
                }
            }
            Ancestry::Parents(ids) => {
                if let Some(first) = ids.first().and_then(|id| s.events.get(id)) {
                    if ids.iter().all(|id| s.events.contains_key(id)) {
                        intent.depends_on = None;
                        intent.parents = ids.clone();
                        intent.event_path = first.path.clone();
                    }
                }
            }
        }
        Ok(())
    }

    /// Begins an edit of `path` with explicit ancestry.
    pub(crate) fn begin_based(
        &self,
        path: &str,
        visible: Option<&Revision>,
        ancestry: &Ancestry,
    ) -> Result<Intent> {
        if self.peer_retention {
            return self.begin_snapshot_based(path, visible, ancestry);
        }
        let mut intent = self.begin_intent(path, visible)?;
        self.apply_ancestry(&mut intent, ancestry)?;
        Ok(intent)
    }

    /// Deletes `path` as a successor of `ancestry` (what the caller last saw).
    pub(crate) fn delete_based(&self, path: &str, ancestry: &Ancestry) -> Result<()> {
        if self.peer_retention {
            return self.delete_snapshot_based(path, ancestry);
        }
        let revision = self
            .view()?
            .get(path)
            .cloned()
            .context("delete source missing")?;
        let mut intent = self.deletion_for(path, &revision)?;
        self.apply_ancestry(&mut intent, ancestry)?;
        self.record_deletion(path, intent)
    }
}
