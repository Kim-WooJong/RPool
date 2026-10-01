//! Quota and capacity reporting of a virtual drive.

use super::*;

impl VirtualDrive {
    pub(crate) fn quota(&self) -> Option<(u64, Option<u64>)> {
        let capacity = self.capacity.lock().ok()?.clone()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if now.saturating_sub(capacity.observed_unix) > 120 || capacity.eligible.is_empty() {
            return None;
        }
        let state = self.state.lock().ok()?;
        let used = state.visible_logical_used().ok()?;
        let pending: Vec<_> = state.pending.iter().map(|i| i.id.clone()).collect();
        let unchanged_events = state.events.keys().eq(capacity.namespace_event_ids.iter());
        let free =
            if unchanged_events && pending == capacity.pending_ids && used == capacity.logical_used
            {
                capacity.additional_estimate
            } else {
                0
            };
        Some((used, Some(used.saturating_add(free))))
    }
    /// Re-measure capacity. It runs beside `sync` rather than behind its lock,
    /// so a long upload cannot leave the snapshot stale. The previous snapshot
    /// keeps serving (and still expires after 120 s) until this one is complete.
    /// A failed refresh clears it, so a failure never looks like a fresh success.
    pub(crate) fn refresh_capacity(&self) -> Result<CapacityStatus> {
        let refreshed = self.measure_capacity();
        *self.capacity.lock().unwrap() = refreshed.as_ref().ok().cloned();
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
