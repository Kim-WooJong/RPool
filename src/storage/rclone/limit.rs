//! Process-wide concurrency caps around single rclone calls (subprocess or
//! daemon). Acquired only at the leaf, around one call, so they cannot
//! deadlock: a holder never waits for another cap of the same kind, and the
//! write cap is always taken after the general cap and released with it.
use super::process;
use crate::storage::error::StorageError;
use crate::storage::traits::OperationContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Concurrent rclone operations per remote name. `RPOOL_RCLONE_PER_REMOTE`.
pub(super) const DEFAULT_PER_REMOTE: usize = 16;
/// Default write cap for Dropbox-backed remotes: Dropbox rejects concurrent
/// writes in one namespace (`too_many_write_operations`).
const DROPBOX_WRITES: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Any,
    Write,
    Read,
}

struct Semaphore {
    limit: usize,
    used: Mutex<usize>,
    freed: Condvar,
}

/// Releases one slot on drop. `None` = uncapped.
pub(super) struct Permit(Option<Arc<Semaphore>>);
impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(semaphore) = self.0.take() {
            let mut used = semaphore
                .used
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *used = used.saturating_sub(1);
            drop(used);
            semaphore.freed.notify_one();
        }
    }
}

/// `0` means uncapped; unset or invalid values use the default.
fn env_limit(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}
fn general_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| env_limit("RPOOL_RCLONE_PER_REMOTE").unwrap_or(DEFAULT_PER_REMOTE))
}
fn write_limit(dropbox: bool) -> usize {
    static LIMIT: OnceLock<Option<usize>> = OnceLock::new();
    LIMIT
        .get_or_init(|| env_limit("RPOOL_RCLONE_WRITES_PER_REMOTE"))
        .unwrap_or(if dropbox {
            DROPBOX_WRITES
        } else {
            general_limit()
        })
}

/// The general per-remote cap (`0` = uncapped).
pub(super) fn general_cap() -> usize {
    general_limit()
}
/// The backend default write cap (`0` = uncapped).
pub(super) fn default_write_cap(dropbox: bool) -> usize {
    write_limit(dropbox)
}

fn acquire_lane(
    lane: Lane,
    key: &str,
    limit: usize,
    ctx: &OperationContext,
) -> Result<Permit, StorageError> {
    if limit == 0 {
        return Ok(Permit(None));
    }
    type Table = HashMap<(Lane, String), Arc<Semaphore>>;
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    let semaphore = TABLE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry((lane, key.to_owned()))
        .or_insert_with(|| {
            Arc::new(Semaphore {
                limit,
                used: Mutex::new(0),
                freed: Condvar::new(),
            })
        })
        .clone();
    let mut used = semaphore
        .used
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while *used >= semaphore.limit {
        process::check(ctx)?;
        used = semaphore
            .freed
            .wait_timeout(used, process::POLL * 5)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
    *used += 1;
    drop(used);
    Ok(Permit(Some(semaphore)))
}

/// Set while a speed test measures uploads at a chosen concurrency: the test
/// itself decides how many calls run, so neither cap may hide the account's
/// real behavior (e.g. Dropbox rejecting a second write).
static UNCAPPED: AtomicBool = AtomicBool::new(false);
/// The calls the speed test runs at once while uncapped (0 = not set).
static TUNING_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Lifts both caps of this process until dropped (speed test tuning only).
pub(crate) struct Uncapped(());
impl Drop for Uncapped {
    fn drop(&mut self) {
        TUNING_CALLS.store(0, Ordering::Release);
        UNCAPPED.store(false, Ordering::Release);
    }
}
/// `calls`: how many run at once, so an account's requests per second are
/// split as the cap `calls` would split them (0 = unknown).
pub(crate) fn uncap_for_speed_test(calls: usize) -> Uncapped {
    TUNING_CALLS.store(calls, Ordering::Release);
    UNCAPPED.store(true, Ordering::Release);
    Uncapped(())
}
fn uncapped() -> bool {
    UNCAPPED.load(Ordering::Acquire)
}
/// The speed test's simultaneous calls while it lifts the caps.
pub(super) fn tuning_calls() -> Option<usize> {
    let calls = TUNING_CALLS.load(Ordering::Acquire);
    (uncapped() && calls > 0).then_some(calls)
}

/// One slot of the general per-remote cap.
pub(super) fn acquire(remote: &str, ctx: &OperationContext) -> Result<Permit, StorageError> {
    if uncapped() {
        return Ok(Permit(None));
    }
    acquire_lane(Lane::Any, remote, general_limit(), ctx)
}

/// One slot of the write cap of the storage namespace `base` (the remote at
/// the bottom of a crypt/alias chain), sized by its backend type.
/// `own` is the account's configured cap (provider limits), which wins over
/// the backend default; a changed cap gets its own semaphore.
pub(super) fn acquire_write(
    base: &str,
    dropbox: bool,
    own: Option<usize>,
    ctx: &OperationContext,
) -> Result<Permit, StorageError> {
    if uncapped() {
        return Ok(Permit(None));
    }
    match own {
        Some(cap) => acquire_lane(Lane::Write, &format!("{base}\u{0}{cap}"), cap, ctx),
        None => acquire_lane(Lane::Write, base, write_limit(dropbox), ctx),
    }
}

/// One slot of the read cap of the storage namespace `base`: `own` is the
/// account's configured cap (provider limits), else the general cap.
pub(super) fn acquire_read(
    base: &str,
    own: Option<usize>,
    ctx: &OperationContext,
) -> Result<Permit, StorageError> {
    if uncapped() {
        return Ok(Permit(None));
    }
    match own {
        Some(cap) => acquire_lane(Lane::Read, &format!("{base}\u{0}{cap}"), cap, ctx),
        None => acquire_lane(Lane::Read, base, general_limit(), ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn semaphore_caps_concurrency_per_key_and_releases_on_drop() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (active, peak) = (active.clone(), peak.clone());
                scope.spawn(move || {
                    for _ in 0..5 {
                        let _permit =
                            acquire_lane(Lane::Any, "limit-test-cap", 3, &OperationContext::none())
                                .unwrap();
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(5));
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        // Another key and the write lane are independent.
        let _a = acquire_lane(Lane::Any, "limit-test-one", 1, &OperationContext::none()).unwrap();
        let _b = acquire_lane(Lane::Write, "limit-test-one", 1, &OperationContext::none()).unwrap();
        let _c = acquire_lane(Lane::Any, "limit-test-two", 1, &OperationContext::none()).unwrap();
        assert!(
            acquire_lane(Lane::Any, "uncapped", 0, &OperationContext::none())
                .unwrap()
                .0
                .is_none()
        );
    }

    #[test]
    fn waiting_for_a_slot_honours_deadline_and_cancellation() {
        let _held =
            acquire_lane(Lane::Write, "limit-test-wait", 1, &OperationContext::none()).unwrap();
        let started = Instant::now();
        let ctx = OperationContext::with_deadline(started + Duration::from_millis(100));
        let error = acquire_lane(Lane::Write, "limit-test-wait", 1, &ctx)
            .err()
            .unwrap();
        assert_eq!(
            error.kind(),
            crate::storage::error::StorageErrorKind::Timeout
        );
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let error = acquire_lane(
            Lane::Write,
            "limit-test-wait",
            1,
            &OperationContext::with_cancel(flag),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.kind(),
            crate::storage::error::StorageErrorKind::Cancelled
        );
        drop(_held);
        acquire_lane(Lane::Write, "limit-test-wait", 1, &OperationContext::none()).unwrap();
    }

    #[test]
    fn dropbox_write_default_is_one() {
        if std::env::var_os("RPOOL_RCLONE_WRITES_PER_REMOTE").is_none() {
            assert_eq!(write_limit(true), 1);
            assert_eq!(write_limit(false), general_limit());
        }
    }
}
