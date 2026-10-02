//! Maintained spool byte count for write admission, replacing a directory
//! scan on every write call. The count is learned by one scan (lazily, at the
//! first admission, so writes made before it are seen), then advanced by every
//! admitted growth. It can only overstate the spool between scans (deletes,
//! truncation, failed writes), so a growth that would exceed the limit is
//! re-checked against a fresh scan before it is refused: a refusal is always
//! exactly what the per-write scan would have decided. A periodic rescan bounds
//! drift from any writer that bypasses admission.
use crate::prelude::*;

/// Admitted growth after which the next admission rescans anyway.
const RESCAN_AFTER: u64 = 1024 * 1024 * 1024;

/// Running spool byte count guarded by `VirtualDrive::spool_writes`.
#[derive(Default)]
pub(crate) struct SpoolMeter {
    /// Spool bytes as last scanned plus admitted growth; `None` before the first scan.
    known: Option<u64>,
    /// Growth admitted since the last scan (rescan after `RESCAN_AFTER`).
    since_scan: u64,
}

impl SpoolMeter {
    /// Replaces the count with a fresh `scan` result.
    fn rescan(&mut self, scan: &dyn Fn() -> Result<u64>) -> Result<u64> {
        let bytes = scan()?;
        self.known = Some(bytes);
        self.since_scan = 0;
        Ok(bytes)
    }
    /// Whether `growth` more spool bytes stay within `limit`. An admitted
    /// growth is counted immediately; call [`Self::invalidate`] if the write
    /// then fails.
    pub(crate) fn admit(
        &mut self,
        growth: u64,
        limit: u64,
        scan: &dyn Fn() -> Result<u64>,
    ) -> Result<bool> {
        if growth == 0 {
            return Ok(true);
        }
        let fits = |bytes: u64| bytes.checked_add(growth).is_some_and(|n| n <= limit);
        let (mut bytes, mut fresh) = match self.known {
            Some(bytes) if self.since_scan < RESCAN_AFTER => (bytes, false),
            _ => (self.rescan(scan)?, true),
        };
        if !fits(bytes) && !fresh {
            bytes = self.rescan(scan)?;
            fresh = true;
        }
        if !fits(bytes) {
            debug_assert!(fresh);
            return Ok(false);
        }
        self.known = Some(bytes + growth);
        self.since_scan = self.since_scan.saturating_add(growth);
        Ok(true)
    }
    #[cfg(test)]
    pub(crate) fn known(&self) -> Option<u64> {
        self.known
    }
    /// Spool files were removed or a write failed: learn the size again.
    pub(crate) fn invalidate(&mut self) {
        self.known = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn counts_admitted_growth_and_rescans_only_before_refusing() {
        let actual = Cell::new(3u64);
        let scans = Cell::new(0);
        let scan = || {
            scans.set(scans.get() + 1);
            Ok(actual.get())
        };
        let mut meter = SpoolMeter::default();
        assert!(meter.admit(2, 10, &scan).unwrap());
        actual.set(5);
        assert!(meter.admit(5, 10, &scan).unwrap());
        actual.set(10);
        assert_eq!(scans.get(), 1);
        // Exceeding the count forces a scan; the scan confirms the refusal.
        assert!(!meter.admit(1, 10, &scan).unwrap());
        assert_eq!(scans.get(), 2);
        // A delete the meter missed is found before refusing.
        actual.set(4);
        assert!(meter.admit(6, 10, &scan).unwrap());
        assert_eq!(scans.get(), 3);
        meter.invalidate();
        actual.set(0);
        assert!(meter.admit(10, 10, &scan).unwrap());
        assert_eq!(scans.get(), 4);
        assert!(meter.admit(0, 0, &scan).unwrap());
        assert!(!meter.admit(u64::MAX, 5, &scan).unwrap());
    }
}
