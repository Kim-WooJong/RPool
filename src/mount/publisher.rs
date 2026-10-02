//! The mounted drive's metadata publisher: publishes committed namespace
//! events to the pool's replicas and cleans committed spool images in the
//! background, woken after each upload commit. Uploads no longer wait for
//! it: before, every upload pass published inline, and new files could not
//! start until all accounts (Dropbox one write at a time) took the records.

use super::virtual_drive::VirtualDrive;
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) fn spawn(
    drive: Arc<VirtualDrive>,
    cancelled: Arc<AtomicBool>,
    interval: Duration,
) -> JoinHandle<()> {
    drive.publisher_running.store(true, Ordering::Release);
    std::thread::spawn(move || {
        run(&drive, &cancelled, interval);
        drive.publisher_running.store(false, Ordering::Release);
    })
}

fn run(drive: &VirtualDrive, cancelled: &AtomicBool, interval: Duration) {
    let mut last_error: Option<String> = None;
    while !cancelled.load(Ordering::Acquire) {
        let seen = drive.publish.generation();
        let result = crate::mount::adoption_fence::check_publish(drive)
            .and_then(|()| drive.publish_and_clean());
        match result {
            Ok(()) => {
                if last_error.take().is_some() {
                    println!("Virtual metadata publication resumed");
                }
            }
            Err(error) => {
                let text = format!("{error:#}");
                if last_error.as_deref() != Some(text.as_str()) {
                    eprintln!("Virtual metadata publication pending: {text}");
                }
                last_error = Some(text);
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        // After a failure, retry within the interval; a commit wakes it now.
        let wait = if last_error.is_some() {
            interval.min(Duration::from_secs(10))
        } else {
            interval
        };
        drive.publish.wait_after(seen, wait);
    }
}
