//! Volume / domain ID types (Step 2 contract).
//!
//! These are runtime/admin resolution facts. They MUST NOT appear as fields in
//! legacy manifest v1/v2 JSON (compatibility.md §2, ADR-001), so they do NOT
//! derive `Serialize`/`Deserialize`. `Generation` is a monotonic counter, not a
//! provider ETag/version, and checks for overflow.
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

use crate::storage::error::StorageError;
use crate::storage::reference::validate_identifier;

/// Monotonic generation counter. NOT a provider ETag/version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub(crate) struct Generation(u64);

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Retained typed volume identity contract; legacy manifests do not serialize domain IDs"
    )
)]
impl Generation {
    /// Wraps a raw counter value.
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw counter value.
    pub(crate) fn value(&self) -> u64 {
        self.0
    }

    /// Checked increment; returns `None` on overflow (`u64::MAX`).
    pub(crate) fn checked_next(&self) -> Option<Generation> {
        self.0.checked_add(1).map(Generation)
    }
}

/// Defines a validated string id newtype (`new` checks it with
/// `validate_identifier`, `as_str` reads it); optional reason text marks it
/// as reserved for future use.
macro_rules! string_id {
    ($name:ident, $label:literal $(, $future:literal)?) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub(crate) struct $name(String);

        $(#[cfg_attr(not(test), expect(dead_code, reason = $future))])?
        impl $name {
            pub(crate) fn new(raw: impl Into<String>) -> Result<Self, StorageError> {
                let raw = raw.into();
                validate_identifier(&raw, $label)?;
                Ok(Self(raw))
            }

            pub(crate) fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(
    VolumeId,
    "volume id",
    "Retained typed volume identity contract; legacy manifests do not serialize domain IDs"
);
string_id!(
    ObjectId,
    "object id",
    "Retained typed volume identity contract; legacy manifests do not serialize domain IDs"
);
string_id!(
    TransactionId,
    "transaction id",
    "Retained typed volume identity contract; legacy manifests do not serialize domain IDs"
);
string_id!(
    ShardId,
    "shard id",
    "Retained typed volume identity contract; legacy manifests do not serialize domain IDs"
);
string_id!(FailureDomainId, "failure domain id");
string_id!(CapacityDomainId, "capacity domain id");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_checked_next_and_overflow() {
        let g = Generation::new(0);
        assert_eq!(g.value(), 0);
        assert_eq!(g.checked_next(), Some(Generation::new(1)));
        assert_eq!(Generation::new(u64::MAX).checked_next(), None);
        assert_eq!(Generation::default().value(), 0);
    }

    #[test]
    fn string_id_validation() {
        assert!(VolumeId::new("").is_err());
        assert!(VolumeId::new("a/b").is_err());
        assert!(VolumeId::new("vol-1").is_ok());
        assert_eq!(VolumeId::new("vol-1").unwrap().as_str(), "vol-1");
        assert_eq!(ObjectId::new("obj-1").unwrap().as_str(), "obj-1");
        assert_eq!(TransactionId::new("tx-1").unwrap().as_str(), "tx-1");
        assert_eq!(ShardId::new("shard-1").unwrap().as_str(), "shard-1");
        assert_eq!(FailureDomainId::new("fd-1").unwrap().as_str(), "fd-1");
        assert!(CapacityDomainId::new("cd-1").is_ok());
    }
}
