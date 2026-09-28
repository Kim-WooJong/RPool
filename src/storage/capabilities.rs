//! Backend capability description (Step 2 contract).
//!
//! `Unknown` is NOT `Supported` (target.md §3.6). A capability that cannot be
//! confirmed is `Unknown` and callers must treat it as unsupported. ETag/version
//! presence does NOT imply conditional-update support.
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

/// Three-state capability. `Unknown` must never be treated as `Supported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    Supported,
    Unsupported,
    Unknown,
}

impl Capability {
    #[cfg_attr(not(test), expect(dead_code, reason = "Backend capability contract remains available before production capability-based routing"))]
    pub(crate) fn is_supported(&self) -> bool {
        matches!(self, Capability::Supported)
    }
}

/// Consistency scope a backend can guarantee. `ProcessLocal` is NOT
/// multi-client safe and must not be advertised as `Strong` (ADR-002/003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "Backend capability contract remains available before production capability-based routing"))]
pub(crate) enum ConsistencyScope {
    /// Linearizable / strong consistency.
    Strong,
    /// Eventually consistent.
    Eventual,
    /// Consistency holds within a single process only (e.g. a Local CAS backed
    /// by a process-local mutex). NOT multi-client safe.
    ProcessLocal,
    /// Unknown; do not assume any consistency.
    Unknown,
}

/// What a backend can do. The safe default is `all_unknown()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BackendCapabilities {
    pub(crate) read: Capability,
    pub(crate) ranged_read: Capability,
    pub(crate) streaming_read: Capability,
    pub(crate) write: Capability,
    pub(crate) overwrite: Capability,
    pub(crate) delete: Capability,
    pub(crate) list: Capability,
    /// Same-backend native copy. Cross-backend copy is a transfer-service
    /// concern, not a backend primitive.
    pub(crate) copy_same_backend: Capability,
    /// Atomic rename. Implementations that cannot provide atomicity must report
    /// `Unsupported` rather than emulate with copy+delete.
    pub(crate) rename: Capability,
    pub(crate) conditional_create: Capability,
    /// Real conditional update (CAS). NOT implied by `version_pinning`.
    pub(crate) conditional_update: Capability,
    pub(crate) conditional_delete: Capability,
    pub(crate) version_pinning: Capability,
    /// Atomic replace (visibility). Distinct from power-loss durability.
    pub(crate) atomic_replace: Capability,
    /// Durability after a write returns (power-loss). Distinct from atomicity.
    pub(crate) durable_after_write: Capability,
    pub(crate) consistency_scope: ConsistencyScope,
    pub(crate) max_object_size: Option<u64>,
    pub(crate) min_part_size: Option<u64>,
    pub(crate) max_part_size: Option<u64>,
}

impl BackendCapabilities {
    /// The safe default: everything unknown. Callers must treat `Unknown` as
    /// unsupported.
    pub(crate) fn all_unknown() -> Self {
        Self {
            read: Capability::Unknown,
            ranged_read: Capability::Unknown,
            streaming_read: Capability::Unknown,
            write: Capability::Unknown,
            overwrite: Capability::Unknown,
            delete: Capability::Unknown,
            list: Capability::Unknown,
            copy_same_backend: Capability::Unknown,
            rename: Capability::Unknown,
            conditional_create: Capability::Unknown,
            conditional_update: Capability::Unknown,
            conditional_delete: Capability::Unknown,
            version_pinning: Capability::Unknown,
            atomic_replace: Capability::Unknown,
            durable_after_write: Capability::Unknown,
            consistency_scope: ConsistencyScope::Unknown,
            max_object_size: None,
            min_part_size: None,
            max_part_size: None,
        }
    }
}

impl Default for BackendCapabilities {
    fn default() -> Self {
        Self::all_unknown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_supported() {
        assert!(!Capability::Unknown.is_supported());
        assert!(!Capability::Unsupported.is_supported());
        assert!(Capability::Supported.is_supported());
    }

    #[test]
    fn default_capabilities_are_all_unknown() {
        let caps = BackendCapabilities::all_unknown();
        assert!(!caps.read.is_supported());
        assert!(!caps.ranged_read.is_supported());
        assert!(!caps.write.is_supported());
        assert!(!caps.conditional_update.is_supported());
        assert!(!caps.rename.is_supported());
        assert_eq!(caps.consistency_scope, ConsistencyScope::Unknown);
        assert!(caps.max_object_size.is_none());
        assert!(caps.min_part_size.is_none());
        assert!(caps.max_part_size.is_none());
    }

    #[test]
    fn process_local_is_not_strong() {
        // A process-local CAS must not be advertised as multi-client safe.
        assert_ne!(ConsistencyScope::ProcessLocal, ConsistencyScope::Strong);
        assert_ne!(ConsistencyScope::Unknown, ConsistencyScope::Strong);
        assert_ne!(ConsistencyScope::Eventual, ConsistencyScope::Strong);
    }
}
