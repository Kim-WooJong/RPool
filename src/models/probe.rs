//! Result of checking one stored shard ([`Probe`]), produced by the storage
//! reader's probes and consumed by scrub, repair and migration.
#[derive(Debug, Clone)]
/// Outcome of probing one shard (size check, or content hash in full mode).
pub(crate) enum Probe {
    /// Present with the expected size (and hash when checked).
    Ok,
    /// Object not found.
    Missing,
    /// Wrong size: `found` vs `expected` bytes.
    BadSize {
        /// Size the provider reported, in bytes.
        found: u64,
        /// Size the manifest records, in bytes.
        expected: u64,
    },
    /// BLAKE3 mismatch: `found` vs `expected` hex digest.
    Corrupt {
        /// BLAKE3 hex digest of the bytes read back.
        found: String,
        /// BLAKE3 hex digest the manifest records.
        expected: String,
    },
    /// Could not be checked (provider/transport error); not proof of loss.
    Error(String),
}

impl Probe {
    /// True only for [`Probe::Ok`].
    pub(crate) fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}
