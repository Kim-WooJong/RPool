//! Concurrent upload rounds on the local fixture drive with a fake uploader.

use super::upload_retry::RetryBook;
use super::*;
use crate::mount::virtual_tests::content;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn write(drive: &VirtualDrive, path: &str, bytes: &[u8]) -> Intent {
    // A newly observed, sequential edit, not an unresolved concurrent editor.
    drive.state.lock().unwrap().bases.clear();
    let intent = drive.begin(path).unwrap();
    fs::write(drive.spool_path(&intent), bytes).unwrap();
    drive.seal(intent).unwrap();
    drive.state.lock().unwrap().pending.last().unwrap().clone()
}
fn pending_paths(drive: &VirtualDrive) -> Vec<String> {
    let state = drive.state.lock().unwrap();
    state.pending.iter().map(|i| i.path.clone()).collect()
}
/// Fake upload: content of the spool file, after `delay`.
fn uploaded(drive: &VirtualDrive, intent: &Intent, delay: Duration) -> Result<Option<Content>> {
    std::thread::sleep(delay);
    Ok(Some(content(&fs::read(drive.spool_path(intent))?)))
}

#[test]
fn unrelated_files_upload_concurrently_and_one_path_stays_in_order() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    drive.upload.set_files(4);
    let first = write(&drive, "a", b"first");
    write(&drive, "b", b"other");
    write(&drive, "c", b"third");
    let second = write(&drive, "a", b"second");
    let (active, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let log = Mutex::new(Vec::new());
    let report = drive
        .upload_pending_with(
            &mut RetryBook::default(),
            &AtomicBool::new(false),
            &|intent| {
                log.lock().unwrap().push(format!("start {}", intent.id));
                peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                let result = uploaded(&drive, intent, Duration::from_millis(150));
                active.fetch_sub(1, Ordering::SeqCst);
                log.lock().unwrap().push(format!("end {}", intent.id));
                result
            },
        )
        .unwrap();
    assert_eq!((report.committed, report.failed), (4, 0));
    assert!(pending_paths(&drive).is_empty());
    assert!(
        peak.load(Ordering::SeqCst) >= 3,
        "a, b and c upload at the same time"
    );
    let log = log.into_inner().unwrap();
    let at = |entry: String| log.iter().position(|e| *e == entry).unwrap();
    assert!(
        at(format!("end {}", first.id)) < at(format!("start {}", second.id)),
        "the second write of a path starts after the first committed: {log:?}"
    );
    // The later write is the visible one.
    let view = drive.view().unwrap();
    let Some(Revision::Cloud { content: c, .. }) = view.get("a") else {
        panic!("a committed");
    };
    assert_eq!(c.size, b"second".len() as u64);
}

#[test]
fn a_failing_upload_does_not_block_unrelated_files() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    write(&drive, "broken", b"x");
    write(&drive, "fine-1", b"y");
    write(&drive, "broken", b"x2");
    write(&drive, "fine-2", b"z");
    let attempts = AtomicUsize::new(0);
    let fake = |intent: &Intent| {
        if intent.path == "broken" {
            attempts.fetch_add(1, Ordering::SeqCst);
            bail!("provider unreachable");
        }
        uploaded(&drive, intent, Duration::ZERO)
    };
    let mut book = RetryBook::default();
    let cancelled = AtomicBool::new(false);
    let report = drive
        .upload_pending_with(&mut book, &cancelled, &fake)
        .unwrap();
    assert_eq!((report.committed, report.failed), (2, 1));
    assert!(report
        .first_error
        .is_some_and(|e| e.contains("broken") && e.contains("provider unreachable")));
    // Both writes of the failing path keep their order; the others are done.
    assert_eq!(pending_paths(&drive), ["broken", "broken"]);
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "tried once per round");
    // Backing off: an immediate next pass does not hammer it.
    let report = drive
        .upload_pending_with(&mut book, &cancelled, &fake)
        .unwrap();
    assert_eq!((report.committed, report.failed), (0, 0));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    // A later file is not held back by the backing-off one.
    write(&drive, "fresh", b"new");
    let report = drive
        .upload_pending_with(&mut book, &cancelled, &fake)
        .unwrap();
    assert_eq!(report.committed, 1);
    assert_eq!(pending_paths(&drive), ["broken", "broken"]);
    // The one-shot `sync` reports what stayed pending, after the rest.
    let error = drive
        .sync_with(&|intent| fake(intent))
        .expect_err("pending failure reported");
    assert!(format!("{error:#}").contains("1 pending upload(s) failed"));
}

#[test]
fn concurrent_files_share_the_pools_shard_workers() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.policy.workers = 2;
    drive.upload.set_files(4);
    for name in ["a", "b", "c", "d", "e", "f"] {
        write(&drive, name, name.as_bytes());
    }
    let (active, peak, files) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let (files_peak, budgets) = (AtomicUsize::new(0), Mutex::new(Vec::new()));
    drive
        .upload_pending_with(
            &mut RetryBook::default(),
            &AtomicBool::new(false),
            &|intent| {
                files_peak.fetch_max(files.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                let budget = crate::storage::transfer_budget::current().expect("budgeted");
                budgets.lock().unwrap().push(budget.clone());
                // Each file runs several "shard transfers" like `put` does.
                std::thread::scope(|scope| {
                    for _ in 0..3 {
                        scope.spawn(|| {
                            let _slot = budget.acquire();
                            peak.fetch_max(
                                active.fetch_add(1, Ordering::SeqCst) + 1,
                                Ordering::SeqCst,
                            );
                            std::thread::sleep(Duration::from_millis(30));
                            active.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                });
                files.fetch_sub(1, Ordering::SeqCst);
                uploaded(&drive, intent, Duration::ZERO)
            },
        )
        .unwrap();
    assert!(files_peak.load(Ordering::SeqCst) >= 2, "files overlap");
    assert!(peak.load(Ordering::SeqCst) <= 2, "never more than workers");
    let budgets = budgets.into_inner().unwrap();
    assert!(budgets.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
}

#[test]
fn cancellation_stops_starting_new_uploads() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    drive.upload.set_files(1);
    write(&drive, "a", b"1");
    write(&drive, "b", b"2");
    let cancelled = AtomicBool::new(false);
    let report = drive
        .upload_pending_with(&mut RetryBook::default(), &cancelled, &|intent| {
            cancelled.store(true, Ordering::SeqCst);
            uploaded(&drive, intent, Duration::ZERO)
        })
        .unwrap();
    assert_eq!(
        report.committed, 1,
        "the running upload finishes, no new one"
    );
    assert_eq!(pending_paths(&drive), ["b"]);
}

#[test]
fn an_acknowledged_write_wakes_the_uploader_at_once() {
    let root = tempfile::tempdir().unwrap();
    let drive = Arc::new(fixture(root.path()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (sent, received) = std::sync::mpsc::channel();
    let worker = {
        let (drive, cancelled) = (drive.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let sent = Mutex::new(sent);
            // A one-hour interval: only the wake-up can start the upload.
            crate::mount::upload_worker::run(
                &drive,
                &cancelled,
                Duration::from_secs(3600),
                &|book, cancelled| {
                    drive
                        .upload_pending_with(book, cancelled, &|intent| {
                            let _ = sent.lock().unwrap().send(Instant::now());
                            uploaded(&drive, intent, Duration::ZERO)
                        })
                        .map(drop)
                },
            )
        })
    };
    // Let the first (empty) pass finish and the uploader go to sleep.
    std::thread::sleep(Duration::from_millis(200));
    let sealed = Instant::now();
    write(&drive, "doc", b"saved");
    let started = received
        .recv_timeout(Duration::from_secs(10))
        .expect("upload started without waiting for the interval");
    assert!(started.duration_since(sealed) < Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pending_paths(&drive).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pending_paths(&drive).is_empty(), "committed");
    cancelled.store(true, Ordering::SeqCst);
    drive.upload.notify();
    worker.join().unwrap();
}

#[test]
fn with_a_publisher_running_an_upload_pass_only_wakes_it() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    write(&drive, "a", b"one");
    drive.publisher_running.store(true, Ordering::Release);
    let before = drive.publish.generation();
    let report = drive
        .upload_pending_with(
            &mut RetryBook::default(),
            &AtomicBool::new(false),
            &|intent| uploaded(&drive, intent, Duration::ZERO),
        )
        .unwrap();
    assert_eq!(report.committed, 1);
    // Woken by the commit and by the end of the pass; publishing and the
    // committed-spool cleanup are left to the publisher.
    assert!(drive.publish.generation() >= before + 2);
    assert!(
        drive.publish_gate.try_lock().is_ok(),
        "pass did not publish"
    );
}

#[test]
fn a_copy_of_a_stored_file_reuses_its_archive_and_edits_stay_separate() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    write(&drive, "a", b"same bytes");
    drive
        .upload_pending_with(
            &mut RetryBook::default(),
            &AtomicBool::new(false),
            &|intent| uploaded(&drive, intent, Duration::ZERO),
        )
        .unwrap();
    let stored = match drive.view().unwrap().get("a").cloned() {
        Some(Revision::Cloud { content, .. }) => content,
        other => panic!("a is not stored: {other:?}"),
    };
    // A copy with identical bytes: the real uploader returns the existing
    // archive without touching any cloud (the fixture has none).
    let copy = write(&drive, "copy-of-a", b"same bytes");
    let reused = drive.upload_intent(&copy).unwrap().unwrap();
    assert_eq!(reused.manifest.archive_id, stored.manifest.archive_id);
    drive.commit_uploaded(&copy, Some(reused)).unwrap();
    // Editing the copy uploads new bytes; the original keeps its archive.
    let edit = write(&drive, "copy-of-a", b"edited bytes");
    let edited = uploaded(&drive, &edit, Duration::ZERO).unwrap();
    drive.commit_uploaded(&edit, edited).unwrap();
    let view = drive.view().unwrap();
    // (The fixture's fake archives share one id, so compare their content.)
    let hash = |p: &str| match &view[p] {
        Revision::Cloud { content, .. } => content.hash.clone(),
        other => panic!("{p}: {other:?}"),
    };
    assert_eq!(hash("a"), stored.hash);
    assert_ne!(hash("copy-of-a"), stored.hash);
    // Different bytes of the same size are not shared.
    let other = write(&drive, "other", b"diff bytes");
    assert!(drive.same_content(&other).unwrap().is_none());
}
