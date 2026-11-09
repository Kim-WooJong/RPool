//! Baseline-compatible local measurement of foreground namespace-lock latency.
//! No mounts, cloud, credentials, or timing-based assertions.
use super::*;
use crate::mount::virtual_tests::content;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn populated(root: &Path, count: usize) -> Arc<VirtualDrive> {
    let drive = fixture(root);
    {
        let mut state = drive.state.lock().unwrap();
        state.version = 6;
        for i in 0..count {
            let event = Event {
                version: 1,
                device: "synthetic-bench".into(),
                worker: "synthetic-bench".into(),
                path: format!("d{}/file-{i}", i / 100),
                parents: vec![],
                content: Some(content(b"data")),
            };
            state.events.insert(event.id().unwrap(), event);
        }
        state.save(root).unwrap();
    }
    Arc::new(drive)
}

fn measured(drive: &Arc<VirtualDrive>, name: &str, f: impl FnOnce()) {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stop = &stop;
        let sampled = scope.spawn(move || {
            let mut waits = Vec::new();
            let mut ready = Some(ready_tx);
            while !stop.load(Ordering::Acquire) {
                let start = Instant::now();
                let state = drive.state.lock().unwrap();
                waits.push(start.elapsed());
                std::hint::black_box(state.events.len());
                drop(state);
                if let Some(tx) = ready.take() {
                    tx.send(()).unwrap();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            waits
        });
        ready_rx.recv().unwrap();
        let start = Instant::now();
        f();
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Release);
        let mut waits = sampled.join().unwrap();
        waits.sort_unstable();
        let max = waits.last().unwrap();
        let p95 = waits[(waits.len() - 1) * 95 / 100];
        eprintln!("projection-lock-bench operation={name} events=8000 elapsed_ms={:.3} foreground_lock_samples={} p95_lock_wait_ms={:.3} max_lock_wait_ms={:.3}",
            elapsed.as_secs_f64()*1000.0, waits.len(), p95.as_secs_f64()*1000.0, max.as_secs_f64()*1000.0);
    });
}

#[test]
#[ignore = "synthetic mutex-latency comparison; no timing pass gate"]
fn projection_foreground_lock_benchmark() {
    let root = tempfile::tempdir().unwrap();
    let drive = populated(root.path(), 8000);
    measured(&drive, "pool_status_3_calls", || {
        for _ in 0..3 {
            assert!(drive.pool_status().unwrap().conflicts.is_empty());
        }
    });
    drive.delete("d0/file-0").unwrap();
    let intent = drive.state.lock().unwrap().pending[0].clone();
    measured(&drive, "commit_uploaded_delete", || {
        drive.commit_uploaded(&intent, None).unwrap();
    });
    assert!(!drive
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .any(|i| i.id == intent.id));
    let reloaded = Namespace::load(root.path(), "synthetic-bench").unwrap();
    assert_eq!(reloaded.events.len(), 8001);
    assert!(!reloaded.resolved().unwrap().contains_key("d0/file-0"));
}
