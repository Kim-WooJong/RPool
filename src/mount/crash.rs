//! Test-only crash points. An armed point fails the current operation at that
//! exact place, as if the process died there; the test then drops the drive
//! and reopens the workspace from disk. Release builds compile these to `Ok`.
use crate::prelude::*;

#[cfg(test)]
thread_local! {
    static ARMED: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// Fails once if `name` is armed on this thread.
#[inline]
pub(crate) fn point(name: &'static str) -> Result<()> {
    #[cfg(test)]
    if ARMED.with(|armed| armed.get() == Some(name)) {
        ARMED.with(|armed| armed.set(None));
        bail!("injected crash at {name}");
    }
    let _ = name;
    Ok(())
}

/// Whether an armed `name` should cut a write short.
#[inline]
pub(crate) fn armed(name: &'static str) -> bool {
    #[cfg(test)]
    if ARMED.with(|armed| armed.get() == Some(name)) {
        return true;
    }
    let _ = name;
    false
}

#[cfg(test)]
pub(crate) fn arm(name: &'static str) {
    ARMED.with(|armed| armed.set(Some(name)));
}
#[cfg(test)]
pub(crate) fn disarm() -> Option<&'static str> {
    ARMED.with(|armed| armed.take())
}

/// Test hook run between the spool fsync and its hash, on the sealing thread.
#[cfg(test)]
pub(crate) mod hash_hook {
    use std::cell::RefCell;
    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
    }
    /// Install `hook` for seals hashed on the current thread.
    pub(crate) fn set(hook: impl FnMut() + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }
    pub(crate) fn clear() {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
    pub(crate) fn run() {
        HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}
