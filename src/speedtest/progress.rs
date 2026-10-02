//! Progress of a speed test.
//!
//! Two channels, both on stderr (stdout carries only the report):
//! - the crate's progress protocol (only when `RPOOL_PROGRESS_PROTOCOL` is
//!   set): one `start` with `remotes × 2 × bytes_per_remote` (upload + read
//!   back), `advance` while bytes move, and `finish`. A remote that fails or
//!   is skipped advances its remaining bytes as completed (not transferred),
//!   so the bar still reaches the end;
//! - plain status lines, one per phase change and at most every
//!   [`TICK_TEXT`] (GUI) / [`TICK_TEXT_TERMINAL`] while files move:
//!
//! ```text
//! Testing <remote> (<i>/<n>): first operation
//! Testing <remote> (<i>/<n>): latency
//! Testing <remote> (<i>/<n>): checking free space
//! Testing <remote> (<i>/<n>): uploading <f>/<F> files, <x>/<y> MiB
//! Testing <remote> (<i>/<n>): downloading <f>/<F> files, <x>/<y> MiB
//! Testing <remote> (<i>/<n>): cleaning up
//! Tested <remote> (<i>/<n>): ok
//! Tested <remote> (<i>/<n>): failed: <one-line error>
//! ```
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How often moved bytes are reported through the protocol.
const TICK_BYTES: Duration = Duration::from_millis(250);
/// Status line interval while transferring, with a GUI parent.
const TICK_TEXT: Duration = Duration::from_secs(2);
/// Status line interval in a terminal.
const TICK_TEXT_TERMINAL: Duration = Duration::from_secs(10);
const MIB: f64 = 1024.0 * 1024.0;

/// Remote position for status lines.
#[derive(Clone, Copy)]
pub(crate) struct Position<'a> {
    pub remote: &'a str,
    pub index: usize,
    pub count: usize,
}

impl Position<'_> {
    pub(crate) fn status(&self, phase: &str) {
        say(&status_line(self, phase));
    }
    pub(crate) fn done(&self, error: Option<&str>) {
        say(&done_line(self, error));
    }
}

/// A closed stderr must not stop the test (or its cleanup).
fn say(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

pub(crate) fn status_line(at: &Position<'_>, phase: &str) -> String {
    format!("Testing {} ({}/{}): {phase}", at.remote, at.index, at.count)
}

pub(crate) fn done_line(at: &Position<'_>, error: Option<&str>) -> String {
    let outcome = match error {
        None => "ok".to_owned(),
        Some(error) => format!("failed: {error}"),
    };
    format!(
        "Tested {} ({}/{}): {outcome}",
        at.remote, at.index, at.count
    )
}

pub(crate) fn transfer_phase(
    verb: &str,
    files_done: usize,
    files: usize,
    moved: u64,
    total: u64,
) -> String {
    format!(
        "{verb} {files_done}/{files} files, {:.1}/{:.1} MiB",
        moved as f64 / MIB,
        total as f64 / MIB
    )
}

/// Live counters of one transfer phase of one remote.
pub(crate) struct Transfer {
    pub moved: AtomicU64,
    pub files_done: AtomicUsize,
    /// Bytes already reported through the protocol.
    reported: AtomicU64,
    /// Moves bytes without reporting them (tuning reports whole levels).
    silent: bool,
}

impl Transfer {
    pub(crate) fn new() -> Self {
        Self {
            moved: AtomicU64::new(0),
            files_done: AtomicUsize::new(0),
            reported: AtomicU64::new(0),
            silent: false,
        }
    }
    /// A transfer that never reports bytes through the protocol.
    pub(crate) fn silent() -> Self {
        Self {
            silent: true,
            ..Self::new()
        }
    }
    /// Reports bytes moved since the last call.
    fn flush_bytes(&self) {
        if self.silent {
            return;
        }
        let moved = self.moved.load(Ordering::Relaxed);
        let before = self.reported.swap(moved, Ordering::Relaxed);
        if moved > before {
            crate::progress::advance(moved - before, moved - before);
        }
    }
    pub(crate) fn reported(&self) -> u64 {
        self.reported.load(Ordering::Relaxed)
    }
    /// Reports moved bytes and status lines until `finished` returns true.
    pub(crate) fn watch(
        &self,
        at: &Position<'_>,
        verb: &str,
        files: usize,
        total: u64,
        finished: impl Fn() -> bool,
    ) {
        let text_every = if crate::progress::enabled() {
            TICK_TEXT
        } else {
            TICK_TEXT_TERMINAL
        };
        let mut last_text = Instant::now();
        let mut last_bytes = Instant::now();
        let line = || {
            transfer_phase(
                verb,
                self.files_done.load(Ordering::Relaxed),
                files,
                self.moved.load(Ordering::Relaxed),
                total,
            )
        };
        at.status(&line());
        while !finished() {
            std::thread::sleep(Duration::from_millis(20));
            if last_bytes.elapsed() >= TICK_BYTES {
                self.flush_bytes();
                last_bytes = Instant::now();
            }
            if last_text.elapsed() >= text_every {
                at.status(&line());
                last_text = Instant::now();
            }
        }
        self.flush_bytes();
    }
}

/// Completes a remote's share of the bar: everything not reported as moved
/// is reported as completed without transfer.
pub(crate) fn settle(expected: u64, reported: u64) {
    if expected > reported {
        crate::progress::advance(expected - reported, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_have_the_documented_shape() {
        let at = Position {
            remote: "c1:rpool",
            index: 2,
            count: 4,
        };
        assert_eq!(
            status_line(&at, "latency"),
            "Testing c1:rpool (2/4): latency"
        );
        assert_eq!(
            status_line(
                &at,
                &transfer_phase("uploading", 1, 4, 6 * 1024 * 1024, 16 * 1024 * 1024)
            ),
            "Testing c1:rpool (2/4): uploading 1/4 files, 6.0/16.0 MiB"
        );
        assert_eq!(done_line(&at, None), "Tested c1:rpool (2/4): ok");
        assert_eq!(
            done_line(&at, Some("upload: timeout")),
            "Tested c1:rpool (2/4): failed: upload: timeout"
        );
    }
}
