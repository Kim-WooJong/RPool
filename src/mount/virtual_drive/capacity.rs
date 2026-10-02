//! Quota and capacity reporting of a virtual drive.

use super::*;

/// How long a capacity snapshot may serve the drive's free-space report.
const SNAPSHOT_MAX_AGE: u64 = 600;

impl VirtualDrive {
    pub(crate) fn quota(&self) -> Option<(u64, Option<u64>)> {
        let capacity = self.capacity.lock().ok()?.clone()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if now.saturating_sub(capacity.observed_unix) > SNAPSHOT_MAX_AGE
            || capacity.eligible.is_empty()
        {
            return None;
        }
        let state = self.state.lock().ok()?;
        let used = state.visible_logical_used().ok()?;
        // Writes since the snapshot no longer zero the free space (Explorer
        // then showed only the used size and refused to copy until the next
        // measurement). Their bytes are charged against the snapshot's
        // estimate instead: every new pending save and every new committed
        // revision counts in full (old versions stay stored), deletions free
        // nothing until cleanup. Uploads still check live quota themselves.
        let known_events: std::collections::BTreeSet<&String> =
            capacity.namespace_event_ids.iter().collect();
        let new_committed: u64 = state
            .events
            .iter()
            .filter(|(id, _)| !known_events.contains(id))
            .filter_map(|(_, event)| event.content.as_ref().map(|c| c.size))
            .sum();
        let new_pending: u64 = state
            .pending
            .iter()
            .filter(|i| i.spool.is_some() && !capacity.pending_ids.contains(&i.id))
            .map(|i| i.size)
            .sum();
        let free = capacity
            .additional_estimate
            .saturating_sub(new_committed.saturating_add(new_pending));
        Some((used, Some(used.saturating_add(free))))
    }
    /// Re-measure capacity. It runs beside `sync` rather than behind its lock,
    /// so a long upload cannot leave the snapshot stale. The previous snapshot
    /// keeps serving (and still expires after 120 s) until this one is complete.
    /// A failed refresh clears it, so a failure never looks like a fresh success.
    pub(crate) fn refresh_capacity(&self) -> Result<CapacityStatus> {
        let refreshed = self.measure_capacity();
        // A failed refresh (one slow or unreachable account) keeps the last
        // good snapshot until it expires (`SNAPSHOT_MAX_AGE`), adjusted by
        // later writes in `quota`, so the drive size does not collapse to
        // the used bytes on one bad measurement.
        if let Ok(status) = &refreshed {
            *self.capacity.lock().unwrap() = Some(status.clone());
        }
        refreshed
    }
    pub(super) fn measure_capacity(&self) -> Result<CapacityStatus> {
        let mut status = CapacityStatus::inspect(
            &crate::storage::admin::RcloneAdmin::inherited(&self.rclone),
            &self.policy,
        )?;
        let state = self.state.lock().unwrap();
        status.committed_logical_used = Some(state.logical_used()?);
        status.logical_used = state.visible_logical_used()?;
        status.usage_scope = "shared-namespace".into();
        status.spool_bytes = self.spool_bytes()?;
        status.spool_limit_bytes = self.spool_limit;
        status.pending_writes = state.pending.len();
        status.pending_ids = state.pending.iter().map(|i| i.id.clone()).collect();
        status.namespace_event_ids = state.events.keys().cloned().collect();
        let pending_sizes: Vec<_> = state
            .pending
            .iter()
            .filter(|i| i.spool.is_some())
            .map(|i| i.size)
            .collect();
        if !self.pool_sync_roots.is_empty() {
            status.pool_sync_roots = self.pool_sync_roots.clone();
            status.conflicts = crate::mount::peer_projection::project(&state.events)?.conflicts;
            status
                .note
                .push_str(" Pool-sync metadata is replicated automatically.");
        }
        drop(state);
        status.reserve_pending(&self.policy, &pending_sizes)?;
        status.logical_ceiling_estimate = status
            .logical_used
            .saturating_add(status.additional_estimate);
        if status.spool_bytes >= status.spool_limit_bytes {
            status.note.push_str(" Local spool budget reached: new growth is rejected; sync or recover retained writes.");
        }
        status.note.push_str(" Virtual mode usage is the known shared namespace plus local pending writes, not the local cache. Other unimported archives are not counted.");
        Ok(status)
    }
}
