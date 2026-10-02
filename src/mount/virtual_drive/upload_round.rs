//! One upload round: pending intents upload a few files at a time.
//!
//! - Up to `UploadControl::files` intents run at once; `upload_queue` decides
//!   which may start (per-path order, `depends_on`, deletions after earlier
//!   writes), so every commit happens in an order a serial upload could have
//!   produced for that path.
//! - All of them share one shard-transfer budget of the pool's `workers`
//!   (`storage::transfer_budget`), so N files never run N x workers shards.
//! - A failing intent is recorded in the caller's `RetryBook` (backoff, one
//!   log line per distinct error) and skipped; unrelated intents continue.
//! - Intents queued while the round runs are picked up within [`POLL`].
//!
//! The caller holds `sync_gate` and publishes afterwards.

use super::upload_queue::next_ready;
use super::upload_retry::RetryBook;
use super::*;
use crate::storage::transfer_budget::{scoped, TransferBudget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Condvar;
use std::time::{Duration, Instant};

/// How often an idle round worker looks for newly queued intents.
const POLL: Duration = Duration::from_millis(200);

/// Uploads one pending intent: `Some(content)` for a write, `None` for a deletion.
pub(crate) type UploadFn<'a> = dyn Fn(&Intent) -> Result<Option<Content>> + Sync + 'a;

/// Outcome of one upload round.
#[derive(Debug, Default)]
pub(crate) struct RoundReport {
    /// Intents uploaded and committed.
    pub committed: usize,
    /// Upload attempts that failed this round.
    pub failed: usize,
    /// `path: error` of the first failure of this round.
    pub first_error: Option<String>,
}

/// Round state shared by the worker threads (under one mutex).
struct Shared<'a> {
    /// Caller's retry book (backoff per intent).
    book: &'a mut RetryBook,
    /// Intent ids currently uploading.
    running: BTreeSet<String>,
    /// Failed in this round: not retried before the next round.
    tried: BTreeSet<String>,
    /// Running totals for the report.
    report: RoundReport,
}

impl VirtualDrive {
    /// Runs one upload round with `UploadControl::files` worker threads sharing
    /// one transfer budget, until nothing more may start; returns the totals.
    pub(super) fn upload_round(
        &self,
        book: &mut RetryBook,
        cancelled: &AtomicBool,
        upload: &UploadFn<'_>,
    ) -> RoundReport {
        {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let ids: BTreeSet<&str> = state.pending.iter().map(|i| i.id.as_str()).collect();
            book.retain(|id| ids.contains(id));
        }
        let budget = Arc::new(TransferBudget::new(self.policy.workers));
        let shared = Mutex::new(Shared {
            book,
            running: BTreeSet::new(),
            tried: BTreeSet::new(),
            report: RoundReport::default(),
        });
        let changed = Condvar::new();
        std::thread::scope(|scope| {
            for _ in 0..self.upload.files() {
                scope.spawn(|| self.round_worker(&shared, &changed, cancelled, upload, &budget));
            }
        });
        shared
            .into_inner()
            .unwrap_or_else(|p| p.into_inner())
            .report
    }

    /// The next intent this worker may upload, or `None` when the round is over.
    fn claim(
        &self,
        shared: &Mutex<Shared<'_>>,
        changed: &Condvar,
        cancelled: &AtomicBool,
    ) -> Option<Intent> {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if cancelled.load(Ordering::Acquire) {
                return None;
            }
            let now = Instant::now();
            let picked = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let sh = &*guard;
                next_ready(&state.pending, |i| {
                    sh.running.contains(&i.id)
                        || sh.tried.contains(&i.id)
                        || !sh.book.due(&i.id, now)
                })
                .map(|index| state.pending[index].clone())
            };
            if let Some(intent) = picked {
                guard.running.insert(intent.id.clone());
                return Some(intent);
            }
            if guard.running.is_empty() {
                // Nothing runs and nothing may start: the round is over.
                changed.notify_all();
                return None;
            }
            guard = changed
                .wait_timeout(guard, POLL)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Worker loop: claims intents, uploads and commits each, wakes the
    /// publisher, and records success or failure in the retry book.
    fn round_worker(
        &self,
        shared: &Mutex<Shared<'_>>,
        changed: &Condvar,
        cancelled: &AtomicBool,
        upload: &UploadFn<'_>,
        budget: &Arc<TransferBudget>,
    ) {
        while let Some(intent) = self.claim(shared, changed, cancelled) {
            // A panic must still release the claim, or the round never ends.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scoped(budget.clone(), || upload(&intent))
                    .and_then(|content| self.commit_uploaded(&intent, content))
                    .inspect(|_| self.publish.notify())
            }))
            .unwrap_or_else(|_| Err(anyhow!("upload worker panicked; local data retained")));
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.running.remove(&intent.id);
            match result {
                Ok(()) => {
                    guard.report.committed += 1;
                    if guard.book.succeed(&intent.id) {
                        println!("Upload of {} completed after retrying", intent.path);
                    }
                }
                Err(error) => {
                    let text = format!("{error:#}");
                    if let Some(delay) = guard.book.fail(&intent.id, text.clone(), Instant::now()) {
                        eprintln!(
                            "Upload of {} pending, retrying in {}s (other files continue): {text}",
                            intent.path,
                            delay.as_secs()
                        );
                    }
                    guard.tried.insert(intent.id.clone());
                    guard.report.failed += 1;
                    guard
                        .report
                        .first_error
                        .get_or_insert_with(|| format!("{}: {text}", intent.path));
                }
            }
            drop(guard);
            changed.notify_all();
        }
    }
}
