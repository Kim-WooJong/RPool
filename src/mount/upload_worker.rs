//! The mounted drive's uploader: one long-lived thread that runs an upload
//! pass (`VirtualDrive::upload_pending`) as soon as a write is acknowledged
//! (`UploadControl::notify`), when a failed upload's backoff elapses, and at
//! least once per background interval as a fallback. Metadata pulls run
//! separately on the interval (`maintenance`), so uploads never wait for one.

use super::virtual_drive::{RetryBook, VirtualDrive};
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(super) fn spawn(
    drive: Arc<VirtualDrive>,
    cancelled: Arc<AtomicBool>,
    interval: Duration,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        run(&drive, &cancelled, interval, &|book, cancelled| {
            drive.upload_pending(book, cancelled).map(drop)
        })
    })
}

/// How long to sleep before the next pass when nothing wakes the uploader.
fn idle_wait(book: &RetryBook, interval: Duration, now: Instant) -> Duration {
    book.next_due(now)
        .map_or(interval, |due| due.saturating_duration_since(now))
        .min(interval)
}

/// One upload pass (`VirtualDrive::upload_pending`; injectable for tests).
pub(super) type Pass<'a> = dyn Fn(&mut RetryBook, &AtomicBool) -> Result<()> + 'a;

pub(super) fn run(
    drive: &VirtualDrive,
    cancelled: &AtomicBool,
    interval: Duration,
    pass: &Pass<'_>,
) {
    let mut book = RetryBook::default();
    let mut last_error: Option<String> = None;
    while !cancelled.load(Ordering::Acquire) {
        // Read before the pass: a write acknowledged during it starts the next one.
        let seen = drive.upload.generation();
        match pass(&mut book, cancelled) {
            Ok(()) => {
                if last_error.take().is_some() {
                    println!("Virtual sync resumed");
                }
                crate::monitor::runtime::note_sync();
            }
            Err(error) => {
                let text = format!("{error:#}");
                if last_error.as_deref() != Some(text.as_str()) {
                    eprintln!("Virtual sync pending: {text}");
                }
                last_error = Some(text);
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        drive
            .upload
            .wait_after(seen, idle_wait(&book, interval, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_wait_honours_backoff_and_interval() {
        let interval = Duration::from_secs(30);
        let now = Instant::now();
        let mut book = RetryBook::default();
        assert_eq!(idle_wait(&book, interval, now), interval);
        book.fail("x", "e".into(), now);
        assert_eq!(idle_wait(&book, interval, now), Duration::from_secs(5));
        assert_eq!(
            idle_wait(&book, Duration::from_secs(2), now),
            Duration::from_secs(2)
        );
    }
}
