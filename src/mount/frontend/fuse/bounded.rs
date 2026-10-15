//! Replies that must arrive before macFUSE gives up on the file system.
//!
//! macFUSE ejects the whole volume when one request stays unanswered longer
//! than the mount's `daemon_timeout` (verified: the request fails with
//! "Socket is not connected" and the session ends; open files lose unsaved
//! data). A read of a file that is not cached waits for its cloud download,
//! and the first write into such a file copies it first, which can take
//! minutes on slow accounts. Such requests therefore run on worker threads
//! (macFUSE serves the session on one thread, so inline they would also stall
//! every other request), queue when all workers are busy, and are answered
//! with `ETIMEDOUT` by a watchdog when they take longer than [`REPLY_LIMIT`]
//! (queue time included). The work itself continues: a download fills the
//! cache, so retrying the read succeeds.
use crate::prelude::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// macFUSE `daemon_timeout` (seconds) RPool mounts with; macFUSE's default
/// is 60 s, too short for one download from a slow account.
pub(super) const DAEMON_TIMEOUT_SECONDS: u64 = 600;
/// Longest a request waits for its answer, queued and running, before the
/// watchdog answers `ETIMEDOUT`; well inside [`DAEMON_TIMEOUT_SECONDS`].
const REPLY_LIMIT: Duration = Duration::from_secs(480);
/// Requests worked on at once; further ones queue (never run inline).
const WORKERS: usize = 16;
/// Longest the session teardown waits for running work.
const TEARDOWN_WAIT: Duration = Duration::from_secs(30);

/// A boxed one-shot task.
type Task = Box<dyn FnOnce() + Send>;

/// Queue, workers and outstanding answers.
#[derive(Default)]
struct State {
    /// Requests waiting for a worker.
    queue: VecDeque<Task>,
    /// Live worker threads.
    workers: usize,
    /// Requests whose work has not finished, by id.
    working: usize,
    /// Outstanding answers by id: deadline and the `ETIMEDOUT` answer.
    deadlines: BTreeMap<u64, (Instant, Task)>,
    /// Next request id.
    next: u64,
}

/// Shared by the runner, its workers and its watchdog.
#[derive(Default)]
struct Inner {
    /// Everything above, under one lock.
    state: Mutex<State>,
    /// Signalled when work finishes or deadlines change.
    changed: Condvar,
}
impl Inner {
    /// Locks the state, ignoring poisoning.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Runner of slow requests with an answer deadline; owned by `RpoolFs`.
pub(super) struct Bounded {
    /// Shared state; the watchdog holds only a weak reference.
    inner: Arc<Inner>,
    /// Answer deadline per request.
    limit: Duration,
}

impl Default for Bounded {
    fn default() -> Self {
        Self::new(REPLY_LIMIT)
    }
}

/// Takes the reply out of `slot` once.
fn take<R>(slot: &Mutex<Option<R>>) -> Option<R> {
    slot.lock().unwrap_or_else(|p| p.into_inner()).take()
}

impl Bounded {
    /// A runner answering `ETIMEDOUT` after `limit`, with its watchdog thread.
    pub(super) fn new(limit: Duration) -> Self {
        let inner = Arc::new(Inner::default());
        let weak = Arc::downgrade(&inner);
        let _ = std::thread::Builder::new()
            .name("rpool-fuse-watchdog".into())
            .spawn(move || watchdog(weak));
        Self { inner, limit }
    }

    /// Runs `work` on a worker and answers with `finish(reply, result)`, or
    /// with `expire(reply)` when the answer deadline passes first (the work
    /// then continues and its result is dropped). A panicking `work` drops
    /// the reply, which answers EIO.
    pub(super) fn run<R, T>(
        &self,
        reply: R,
        expire: impl FnOnce(R) + Send + 'static,
        work: impl FnOnce() -> T + Send + 'static,
        finish: impl FnOnce(R, T) + Send + 'static,
    ) where
        R: Send + 'static,
        T: 'static,
    {
        let slot = Arc::new(Mutex::new(Some(reply)));
        let expired = slot.clone();
        let inner = self.inner.clone();
        let mut state = self.inner.lock();
        let id = state.next;
        state.next += 1;
        state.deadlines.insert(
            id,
            (
                Instant::now() + self.limit,
                Box::new(move || {
                    if let Some(reply) = take(&expired) {
                        expire(reply);
                    }
                }),
            ),
        );
        state.working += 1;
        state.queue.push_back(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let reply = take(&slot);
            if let (Some(reply), Ok(result)) = (reply, result) {
                finish(reply, result);
            }
            let mut state = inner.lock();
            state.deadlines.remove(&id);
            state.working -= 1;
            inner.changed.notify_all();
        }));
        if state.workers < WORKERS {
            state.workers += 1;
            let inner = self.inner.clone();
            let spawned = std::thread::Builder::new()
                .name("rpool-fuse-io".into())
                .spawn(move || worker(&inner));
            if spawned.is_err() {
                state.workers -= 1;
                if state.workers == 0 {
                    // No thread at all: run the queue here rather than never.
                    drop(state);
                    worker_without_count(&self.inner);
                    return;
                }
            }
        }
        self.inner.changed.notify_all();
    }

    /// Waits (at most [`TEARDOWN_WAIT`]) until no request is being worked on.
    pub(super) fn wait_idle(&self) {
        let deadline = Instant::now() + TEARDOWN_WAIT;
        let mut state = self.inner.lock();
        while state.working > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            state = self
                .inner
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// Runs queued tasks until the queue is empty, then retires.
fn worker(inner: &Inner) {
    loop {
        let task = {
            let mut state = inner.lock();
            match state.queue.pop_front() {
                Some(task) => task,
                None => {
                    state.workers -= 1;
                    return;
                }
            }
        };
        task();
    }
}

/// Runs the queue on the calling thread (no worker could be started).
fn worker_without_count(inner: &Inner) {
    loop {
        let task = inner.lock().queue.pop_front();
        match task {
            Some(task) => task(),
            None => return,
        }
    }
}

/// Answers requests past their deadline; exits once the runner is gone.
fn watchdog(inner: Weak<Inner>) {
    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let now = Instant::now();
        let mut state = inner.lock();
        let due: Vec<u64> = state
            .deadlines
            .iter()
            .filter(|(_, (deadline, _))| *deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        let expired: Vec<Task> = due
            .iter()
            .filter_map(|id| state.deadlines.remove(id))
            .map(|(_, task)| task)
            .collect();
        let next = state
            .deadlines
            .values()
            .map(|(deadline, _)| deadline.saturating_duration_since(now))
            .min()
            .unwrap_or(Duration::MAX)
            .min(Duration::from_secs(1));
        if expired.is_empty() {
            drop(
                inner
                    .changed
                    .wait_timeout(state, next)
                    .unwrap_or_else(|p| p.into_inner()),
            );
        } else {
            drop(state);
            eprintln!(
                "{} drive request(s) took too long (usually a slow cloud download); answered with a timeout error so macOS keeps the drive mounted. The download continues: open the file again later.",
                expired.len()
            );
            for task in expired {
                task();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn slow_work_is_answered_by_the_deadline_and_others_still_finish() {
        let runner = Bounded::new(Duration::from_millis(300));
        let (answers, received) = mpsc::channel::<(&str, &str)>();
        let (release, blocked) = mpsc::channel::<()>();
        let blocked = Arc::new(Mutex::new(blocked));
        // More stuck requests than workers: the extra ones queue and are
        // answered by the deadline too, and the session thread never blocks.
        for _ in 0..WORKERS + 4 {
            let (answer, expired, blocked) = (answers.clone(), answers.clone(), blocked.clone());
            runner.run(
                "stuck",
                move |r| expired.send((r, "timeout")).unwrap(),
                move || {
                    let _ = blocked.lock().unwrap().recv_timeout(Duration::from_secs(5));
                },
                move |r, ()| answer.send((r, "done")).unwrap(),
            );
        }
        let started = Instant::now();
        let mut timeouts = 0;
        for _ in 0..WORKERS + 4 {
            let (_, how) = received.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(how, "timeout");
            timeouts += 1;
        }
        assert_eq!(timeouts, WORKERS + 4);
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(release);
        // A quick request gets its real answer, exactly once.
        let answer = answers.clone();
        let expired = answers.clone();
        runner.run(
            "quick",
            move |r| expired.send((r, "timeout")).unwrap(),
            || 7,
            move |r, n: i32| {
                assert_eq!(n, 7);
                answer.send((r, "done")).unwrap()
            },
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            ("quick", "done")
        );
        runner.wait_idle();
        assert!(received.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn a_panicking_request_drops_its_reply_once() {
        let runner = Bounded::new(Duration::from_secs(60));
        let (dropped, seen) = mpsc::channel::<()>();
        /// Signals when dropped (like a fuser reply answering EIO).
        struct Reply(mpsc::Sender<()>);
        impl Drop for Reply {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        runner.run(
            Reply(dropped),
            |_| panic!("not expired"),
            || panic!("work fails"),
            |_, ()| unreachable!(),
        );
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        runner.wait_idle();
    }
}
