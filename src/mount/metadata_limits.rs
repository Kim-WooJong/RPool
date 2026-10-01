//! One home for the pool-metadata size bounds of the v6 readers.

/// Largest single metadata object (record, checkpoint chunk, head or mark).
pub(crate) const RECORD_BYTES_MAX: usize = 8 * 1024 * 1024;
/// The pre-checkpoint bootstrap budget. v6 uses it as
/// the page size of a streamed read, and older RPool versions fail beyond it.
pub(crate) const LEGACY_BOOTSTRAP_RECORDS: usize = 10_000;
pub(crate) const LEGACY_BOOTSTRAP_BYTES: usize = 64 * 1024 * 1024;
/// Streamed (paged) reads: one page at most holds this many raw bytes.
pub(crate) const PAGE_RECORDS: usize = LEGACY_BOOTSTRAP_RECORDS;
pub(crate) const PAGE_BYTES: usize = LEGACY_BOOTSTRAP_BYTES;
/// Safety ceiling for one pull of unseen records (individual objects).
pub(crate) const STREAM_RECORDS_MAX: usize = 1_000_000;
pub(crate) const STREAM_BYTES_MAX: u64 = 16 * 1024 * 1024 * 1024;
/// Warn when a pool reaches this share of the legacy budget (80 %).
pub(crate) const WARN_RECORDS: usize = LEGACY_BOOTSTRAP_RECORDS / 10 * 8;
pub(crate) const WARN_BYTES: u64 = LEGACY_BOOTSTRAP_BYTES as u64 / 10 * 8;

/// Error for a pull above the streaming ceiling, with what to do about it.
pub(crate) fn ceiling_message(records: usize, bytes: u64) -> String {
    format!(
        "pool metadata has {records} unseen records ({} MiB) without a checkpoint, above the \
         streaming ceiling of {STREAM_RECORDS_MAX} records / {} GiB; keep this workspace and run \
         `rpool pool compact <NAME>` on a PC that can open the drive, then retry",
        bytes / (1024 * 1024),
        STREAM_BYTES_MAX / (1024 * 1024 * 1024)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warning_level_is_below_the_legacy_budget() {
        assert_eq!(WARN_RECORDS, 8_000);
        assert!(WARN_BYTES < LEGACY_BOOTSTRAP_BYTES as u64);
        assert!(ceiling_message(5, 0).contains("rpool pool compact"));
    }
}
