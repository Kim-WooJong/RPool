//! Runs sealing replies (`release`, `fsync`) off the FUSE request loop. A seal
//! flushes and hashes the whole file, and macFUSE serves the session on one
//! thread, so an inline seal of a large file would stall every other request.
//! The reply is still sent only after the seal finishes, so the durability
//! acknowledgement is unchanged. Past `LIMIT` concurrent seals a job runs
//! inline (back-pressure instead of unbounded threads).
use crate::prelude::*;
use std::sync::Condvar;

/// Most seals run on worker threads at once; further jobs run inline.
const LIMIT: usize = 16;

/// Boxed seal job.
type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
/// Bounded runner of detached seal jobs, owned by `RpoolFs`.
pub(super) struct Detached {
    /// Number of running jobs and a condvar signalled when one finishes.
    running: Arc<(Mutex<usize>, Condvar)>,
}

/// Counts one running job; dropping it (even on a panic) ends the count.
struct Running(Arc<(Mutex<usize>, Condvar)>);
impl Drop for Running {
    fn drop(&mut self) {
        let (count, idle) = &*self.0;
        let mut count = count.lock().unwrap_or_else(|p| p.into_inner());
        *count -= 1;
        idle.notify_all();
    }
}

impl Detached {
    /// Runs `job` on a new `rpool-fuse-seal` thread, or inline when `LIMIT` jobs already run
    /// or the thread cannot be spawned.
    pub(super) fn run(&self, job: impl FnOnce() + Send + 'static) {
        {
            let mut count = self.running.0.lock().unwrap_or_else(|p| p.into_inner());
            if *count >= LIMIT {
                drop(count);
                return job();
            }
            *count += 1;
        }
        let running = Running(self.running.clone());
        let job: Arc<Mutex<Option<Job>>> = Arc::new(Mutex::new(Some(Box::new(job))));
        let theirs = job.clone();
        let spawned = std::thread::Builder::new()
            .name("rpool-fuse-seal".into())
            .spawn(move || {
                let _running = running;
                let job = theirs.lock().unwrap_or_else(|p| p.into_inner()).take();
                if let Some(job) = job {
                    job();
                }
            });
        if spawned.is_err() {
            // The closure (and its count) was dropped unrun: run it here.
            let job = job.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(job) = job {
                job();
            }
        }
    }
    /// Wait until every detached job has replied (session teardown).
    pub(super) fn wait_idle(&self) {
        let (count, idle) = &*self.running;
        let mut count = count.lock().unwrap_or_else(|p| p.into_inner());
        while *count > 0 {
            count = idle.wait(count).unwrap_or_else(|p| p.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn jobs_run_off_the_caller_and_teardown_waits_for_them() {
        let detached = Detached::default();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let caller = std::thread::current().id();
        detached.run(move || {
            go_rx.recv().unwrap();
            done_tx.send(std::thread::current().id()).unwrap();
        });
        // `run` returned while the job is still blocked: the loop is free.
        go_tx.send(()).unwrap();
        detached.wait_idle();
        assert_ne!(done_rx.try_recv().unwrap(), caller);
    }

    #[test]
    fn past_the_limit_jobs_run_inline() {
        let detached = Detached::default();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Arc::new(Mutex::new(go_rx));
        for _ in 0..LIMIT {
            let go_rx = go_rx.clone();
            detached.run(move || {
                let _ = go_rx.lock().unwrap().recv();
            });
        }
        let caller = std::thread::current().id();
        let (tx, rx) = mpsc::channel();
        detached.run(move || tx.send(std::thread::current().id()).unwrap());
        assert_eq!(rx.try_recv().unwrap(), caller);
        drop(go_tx);
        detached.wait_idle();
    }
}
