//! A transfer budget shared by several concurrent `put`s. The virtual drive
//! uploads a few files at once; each `put` keeps its own `workers` scheduler,
//! but every shard write first takes a slot here, so the total number of
//! running shard transfers never exceeds the pool's `workers`.
//!
//! The budget reaches `put` through a thread-scoped handle ([`scoped`]),
//! so plain `rpool put` and every other caller stay unbudgeted and unchanged.

use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex};

pub(crate) struct TransferBudget {
    free: Mutex<usize>,
    released: Condvar,
}
impl TransferBudget {
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            free: Mutex::new(slots.max(1)),
            released: Condvar::new(),
        }
    }
    /// Blocks until a slot is free; the slot returns when the guard drops.
    pub(crate) fn acquire(&self) -> Slot<'_> {
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        while *free == 0 {
            free = self.released.wait(free).unwrap_or_else(|p| p.into_inner());
        }
        *free -= 1;
        Slot(self)
    }
    #[cfg(test)]
    pub(crate) fn free(&self) -> usize {
        *self.free.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub(crate) struct Slot<'a>(&'a TransferBudget);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut free = self.0.free.lock().unwrap_or_else(|p| p.into_inner());
        *free += 1;
        self.0.released.notify_one();
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<TransferBudget>>> = const { RefCell::new(None) };
}

/// Runs `f` with `budget` as this thread's transfer budget.
pub(crate) fn scoped<R>(budget: Arc<TransferBudget>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<TransferBudget>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|current| *current.borrow_mut() = previous);
        }
    }
    let previous = CURRENT.with(|current| current.borrow_mut().replace(budget));
    let _restore = Restore(previous);
    f()
}

/// The budget of the calling thread, if a [`scoped`] caller set one.
pub(crate) fn current() -> Option<Arc<TransferBudget>> {
    CURRENT.with(|current| current.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn concurrent_holders_never_exceed_the_budget() {
        let budget = Arc::new(TransferBudget::new(3));
        let (active, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    let _slot = budget.acquire();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert_eq!(budget.free(), 3);
    }

    #[test]
    fn the_scope_is_per_thread_and_restored() {
        assert!(current().is_none());
        let budget = Arc::new(TransferBudget::new(2));
        scoped(budget.clone(), || {
            assert!(current().is_some_and(|b| Arc::ptr_eq(&b, &budget)));
            std::thread::spawn(|| assert!(current().is_none()))
                .join()
                .unwrap();
        });
        assert!(current().is_none());
    }
}
