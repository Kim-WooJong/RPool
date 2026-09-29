//! Work-conserving, round-robin transfer dispatch. Waiting/retrying jobs hold no
//! worker slot. The coordinator owns completion callbacks and persistent state.
use super::error::StorageError;
use crate::prelude::*;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) fn remote_key(remote: &str) -> String {
    remote
        .split_once(':')
        .map_or(remote, |(name, _)| name)
        .to_owned()
}

pub(crate) fn read_retry(error: &anyhow::Error, attempt: u32) -> Option<Duration> {
    let error = error.downcast_ref::<StorageError>()?;
    error.is_retriable().then(|| {
        error
            .retry_after()
            .unwrap_or_default()
            .max(Duration::from_millis(
                100u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(6)),
            ))
    })
}

struct Pending<T> {
    job: T,
    attempt: u32,
    ready: Instant,
    followup: bool,
}

/// `complete` is called for every terminal result (including already-running
/// jobs after a fatal callback). Returning jobs supports a bounded producer.
/// Retry policy is explicit: mutation pipelines MUST NOT blindly retry a whole
/// write+readback transaction. Only safe read tasks opt into `read_retry`.
pub(crate) fn run<T: Send, R: Send>(
    jobs: Vec<T>,
    workers: usize,
    attempts: u32,
    key: impl Fn(&T) -> String,
    execute: impl Fn(&T) -> Result<R> + Sync,
    retry: impl Fn(&anyhow::Error, u32) -> Option<Duration>,
    mut complete: impl FnMut(T, Result<R>) -> Result<Vec<T>>,
) -> Result<()> {
    if workers == 0 {
        bail!("scheduler workers must be positive");
    }
    if jobs.is_empty() {
        return Ok(());
    }
    let mut queues: BTreeMap<String, VecDeque<Pending<T>>> = BTreeMap::new();
    let mut order = VecDeque::new();
    let enqueue = |job: T,
                   queues: &mut BTreeMap<String, VecDeque<Pending<T>>>,
                   order: &mut VecDeque<String>,
                   followup: bool| {
        let domain = key(&job);
        if !queues.contains_key(&domain) {
            order.push_back(domain.clone());
        }
        queues.entry(domain).or_default().push_back(Pending {
            job,
            attempt: 1,
            ready: Instant::now(),
            followup,
        });
    };
    for job in jobs {
        enqueue(job, &mut queues, &mut order, false);
    }
    // Keep at least two domains progressing even when one provider is slow.
    // One-domain jobs may use the full budget; multi-domain queues use <= half.
    let thread_count = workers;
    let mut last_followup = BTreeMap::<String, bool>::new();
    let (work_tx, work_rx) = mpsc::sync_channel::<(String, Pending<T>)>(thread_count);
    let work_rx = Mutex::new(work_rx);
    let (done_tx, done_rx) = mpsc::channel();
    let mut active = BTreeMap::<String, usize>::new();
    let mut running = 0;
    let mut fatal = None;
    let operation = crate::storage::traits::OperationContext::none();
    std::thread::scope(|scope| {
        for _ in 0..thread_count {
            let work_rx = &work_rx;
            let done_tx = done_tx.clone();
            let execute = &execute;
            scope.spawn(move || loop {
                let next = work_rx.lock().expect("scheduler receiver poisoned").recv();
                let Ok((domain, pending)) = next else {
                    break;
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&pending.job)
                }))
                .unwrap_or_else(|_| Err(anyhow!("transfer worker panicked")));
                if done_tx.send((domain, pending, result)).is_err() {
                    break;
                }
            });
        }
        loop {
            // Stop dispatch/retry waits, but drain running results through complete
            // so verified writes still get their durable journal receipts.
            if fatal.is_none() && operation.is_cancelled() {
                fatal = Some(
                    crate::storage::error::StorageError::Cancelled {
                        detail: "transfer scheduling cancelled".into(),
                    }
                    .into(),
                );
            }
            if fatal.is_none() {
                while running < thread_count {
                    let mut selected = None;
                    for _ in 0..order.len() {
                        let domain = order.pop_front().expect("nonempty domain ring");
                        order.push_back(domain.clone());
                        let live_domains = queues
                            .iter()
                            .filter(|(k, q)| {
                                k.as_str() != "\0parity-encoder"
                                    && (!q.is_empty() || active.get(*k).copied().unwrap_or(0) > 0)
                            })
                            .count();
                        let per_remote = if domain == "\0parity-encoder" {
                            1
                        } else if live_domains == 1 {
                            workers
                        } else {
                            workers.div_ceil(2)
                        };
                        if active.get(&domain).copied().unwrap_or(0) >= per_remote {
                            continue;
                        }
                        let queue = queues.get_mut(&domain).expect("domain exists");
                        let last = last_followup.get(&domain).copied().unwrap_or(false);
                        let eligible = |p: &Pending<T>| p.ready <= Instant::now();
                        let index = queue
                            .iter()
                            .position(|p| eligible(p) && p.followup != last)
                            .or_else(|| queue.iter().position(eligible));
                        if let Some(index) = index {
                            last_followup.insert(domain.clone(), queue[index].followup);
                            selected = Some((domain, queue.remove(index).unwrap()));
                            break;
                        }
                    }
                    let Some((domain, pending)) = selected else {
                        break;
                    };
                    *active.entry(domain.clone()).or_default() += 1;
                    running += 1;
                    work_tx
                        .send((domain, pending))
                        .expect("workers retain receiver");
                }
            }
            let queued = queues.values().any(|q| !q.is_empty());
            if running == 0 && (fatal.is_some() || !queued) {
                break;
            }
            let next_ready = queues
                .values()
                .flat_map(|q| q.iter())
                .filter_map(|p| (p.ready > Instant::now()).then_some(p.ready))
                .min();
            let wait = next_ready
                .map(|at| at.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(1));
            let received = done_rx.recv_timeout(wait.min(Duration::from_millis(50)));
            let Ok((domain, mut pending, result)) = received else {
                continue;
            };
            running -= 1;
            *active.get_mut(&domain).unwrap() -= 1;
            if fatal.is_none() && pending.attempt < attempts.max(1) {
                if let Err(error) = &result {
                    if let Some(delay) = retry(error, pending.attempt) {
                        pending.attempt += 1;
                        pending.ready = Instant::now()
                            .checked_add(delay)
                            .unwrap_or_else(Instant::now);
                        queues.get_mut(&domain).unwrap().push_back(pending);
                        continue;
                    }
                }
            }
            match complete(pending.job, result) {
                Ok(new_jobs) if fatal.is_none() => {
                    for job in new_jobs {
                        enqueue(job, &mut queues, &mut order, true);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    // Cancellation stops dispatch, but says nothing about already
                    // running mutations. Preserve uncertainty over any other
                    // terminal error, and real failures over a clean cancellation.
                    let priority = |error: &anyhow::Error| match error
                        .downcast_ref::<crate::storage::error::StorageError>()
                        .map(crate::storage::error::StorageError::kind)
                    {
                        Some(crate::storage::error::StorageErrorKind::UnknownOutcome) => 2,
                        Some(crate::storage::error::StorageErrorKind::Cancelled) => 0,
                        _ => 1,
                    };
                    if fatal
                        .as_ref()
                        .map_or(true, |previous| priority(&error) > priority(previous))
                    {
                        fatal = Some(error);
                    }
                }
            }
        }
        drop(work_tx);
    });
    if let Some(error) = fatal {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn completed_domains_do_not_limit_remaining_tail() {
        let barrier = std::sync::Barrier::new(4);
        let mut seeds = 0;
        run(
            vec![0, 1],
            4,
            1,
            |job| if *job == 1 { "b".into() } else { "a".into() },
            |job| {
                if *job >= 2 {
                    barrier.wait();
                }
                Ok(())
            },
            |_, _| None,
            |job, result| {
                result?;
                if job < 2 {
                    seeds += 1;
                }
                Ok(if job < 2 && seeds == 2 {
                    vec![2, 3, 4, 5]
                } else {
                    vec![]
                })
            },
        )
        .unwrap();
    }

    #[test]
    fn one_seed_can_expand_to_full_worker_capacity() {
        let barrier = std::sync::Barrier::new(4);
        run(
            vec![0],
            4,
            1,
            |_| "same".into(),
            |job| {
                if *job != 0 {
                    barrier.wait();
                }
                Ok(())
            },
            |_, _| None,
            |job, result| {
                result?;
                Ok(if job == 0 { vec![1, 2, 3, 4] } else { vec![] })
            },
        )
        .unwrap();
    }

    #[test]
    fn global_and_remote_caps_and_cross_remote_progress() {
        let active = AtomicUsize::new(0);
        let per = [AtomicUsize::new(0), AtomicUsize::new(0)];
        let seen = Mutex::new(BTreeSet::new());
        let barrier = std::sync::Barrier::new(4);
        run(
            (0..12).map(|i| i % 2).collect(),
            4,
            1,
            |d| d.to_string(),
            |d| {
                assert!(active.fetch_add(1, Ordering::SeqCst) < 4);
                assert!(per[*d].fetch_add(1, Ordering::SeqCst) < 2);
                seen.lock().unwrap().insert(*d);
                barrier.wait();
                per[*d].fetch_sub(1, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
            |_, _| None,
            |_, r| {
                r?;
                Ok(vec![])
            },
        )
        .unwrap();
        assert_eq!(seen.into_inner().unwrap().len(), 2);
    }

    #[test]
    fn retry_releases_slot_and_dynamic_jobs_work_with_one_worker() {
        let calls = Mutex::new(Vec::new());
        let first = AtomicUsize::new(0);
        run(
            vec![0, 1],
            1,
            2,
            |_| "same".into(),
            |job| {
                calls.lock().unwrap().push(*job);
                if *job == 0 && first.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(StorageError::Timeout {
                        detail: "synthetic".into(),
                    }
                    .into());
                }
                Ok(())
            },
            read_retry,
            |job, r| {
                r?;
                Ok(if job == 1 { vec![2] } else { vec![] })
            },
        )
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![0, 1, 2, 0]);
    }
}
