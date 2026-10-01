//! StorageBackend contract (Step 2).
//!
//! Object-safe, synchronous, streaming. No generic methods, no `Self` returns,
//! no `impl Trait` in signatures — `dyn StorageBackend` must work. No
//! rclone/OpenDAL/GUI/Manifest types appear here (target.md §3.3).
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::storage::capabilities::BackendCapabilities;
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};

/// Half-open byte range `[offset, offset + length)` with checked arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadRange {
    offset: u64,
    length: u64,
}

impl ReadRange {
    pub(crate) fn new(offset: u64, length: u64) -> Result<Self, StorageError> {
        offset
            .checked_add(length)
            .map(|_| Self { offset, length })
            .ok_or_else(|| {
                StorageError::invalid_input(format!(
                    "range overflow: offset={offset} + length={length}"
                ))
            })
    }

    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// `offset + length`. Safe: `new` guarantees no overflow.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    pub(crate) fn end(&self) -> u64 {
        self.offset + self.length
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.length == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectMetadata {
    pub(crate) size: u64,
    pub(crate) is_dir: bool,
    /// Opaque version/ETag if the backend exposes one. Presence does NOT imply
    /// conditional-update support (target.md §3.6).
    pub(crate) version: Option<String>,
    /// Provider modification time as reported (opaque text), if any. Used only
    /// to notice that an object changed since this process verified it.
    pub(crate) modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadReceipt {
    pub(crate) bytes_read: u64,
    pub(crate) version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteOptions {
    /// If true, overwrite an existing object. If false and the object exists,
    /// the write must fail with `AlreadyExists` (conditional create).
    pub(crate) overwrite: bool,
    /// Conditional update: only write if the current version matches.
    pub(crate) expected_version: Option<String>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            overwrite: true,
            expected_version: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteReceipt {
    pub(crate) size: u64,
    pub(crate) version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyReceipt {
    pub(crate) size: u64,
    pub(crate) version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListEntry {
    pub(crate) key: ObjectKey,
    pub(crate) size: u64,
    pub(crate) is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListPage {
    pub(crate) entries: Vec<ListEntry>,
    pub(crate) next_page: Option<String>,
}

// Only installed by a dedicated mount CLI process. Never install this in the GUI:
// its independent jobs run in their own child processes. A process scope is needed
// because upload scheduling creates threads that do not inherit thread-local state.
static PROCESS_CANCELLATION: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

pub(crate) struct ProcessCancellationGuard {
    cancel: Arc<AtomicBool>,
}
impl ProcessCancellationGuard {
    pub(crate) fn install(cancel: Arc<AtomicBool>) -> anyhow::Result<Self> {
        let mut slot = PROCESS_CANCELLATION
            .lock()
            .map_err(|_| anyhow::anyhow!("process cancellation lock poisoned"))?;
        anyhow::ensure!(slot.is_none(), "process cancellation already installed");
        *slot = Some(cancel.clone());
        Ok(Self { cancel })
    }
}
impl Drop for ProcessCancellationGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = PROCESS_CANCELLATION.lock() {
            if slot
                .as_ref()
                .is_some_and(|flag| Arc::ptr_eq(flag, &self.cancel))
            {
                *slot = None;
            }
        }
    }
}
fn inherited_cancellation() -> Vec<Arc<AtomicBool>> {
    PROCESS_CANCELLATION
        .lock()
        .expect("process cancellation lock poisoned")
        .iter()
        .cloned()
        .collect()
}

/// Per-operation deadline + cancellation, propagated into adapters. A remote
/// write that may have committed after a cancel/timeout is `UnknownOutcome`,
/// not a clean cancel (ADR-004).
#[derive(Clone)]
pub(crate) struct OperationContext {
    deadline: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
    child_cancels: Vec<Arc<AtomicBool>>,
}

impl OperationContext {
    pub(crate) fn none() -> Self {
        Self {
            deadline: None,
            cancel: None,
            child_cancels: inherited_cancellation(),
        }
    }

    pub(crate) fn with_deadline(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
            cancel: None,
            child_cancels: inherited_cancellation(),
        }
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    pub(crate) fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: None,
            cancel: Some(cancel),
            child_cancels: inherited_cancellation(),
        }
    }

    pub(crate) fn with_deadline_and_cancel(deadline: Instant, cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Some(deadline),
            cancel: Some(cancel),
            child_cancels: inherited_cancellation(),
        }
    }

    pub(crate) fn child(&self, cancel: Arc<AtomicBool>) -> Self {
        let mut child = self.clone();
        child.child_cancels.push(cancel);
        child
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Acquire))
            || self.child_cancels.iter().any(|c| c.load(Ordering::Acquire))
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether the deadline has passed. A passed deadline does NOT by itself
    /// abort an in-flight remote write.
    pub(crate) fn deadline_passed(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

impl Default for OperationContext {
    fn default() -> Self {
        Self::none()
    }
}

/// The storage contract. Object-safe: no generic methods, no `Self` returns, no
/// `impl Trait` in signatures. `Send + Sync` so `Arc<dyn StorageBackend>` can
/// cross Rayon worker threads.
pub(crate) trait StorageBackend: Send + Sync {
    fn id(&self) -> BackendId;

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    fn capabilities(&self) -> BackendCapabilities;

    fn stat(&self, ctx: &OperationContext, key: &ObjectKey)
        -> Result<ObjectMetadata, StorageError>;

    /// Stream `range` bytes of `key` into `sink`. Implementations must not load
    /// the whole object into memory unless the range is the whole object.
    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError>;

    /// Convenience: read at most `limit` bytes into a `Vec`. `None` means the
    /// whole object; callers must pass an explicit limit for metadata reads.
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError>;

    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError>;

    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError>;

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    fn list(
        &self,
        ctx: &OperationContext,
        prefix: &str,
        page: Option<&str>,
    ) -> Result<ListPage, StorageError>;

    /// Same-backend native copy. Cross-backend copy is a transfer-service
    /// concern (read → write → verify), not a backend primitive.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    fn copy(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError>;

    /// Optional atomic rename. Implementations that cannot provide atomicity
    /// must return `Unsupported` rather than emulate with copy+delete.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained backend contract; production callers currently use the legacy operation subset"
        )
    )]
    fn rename(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<(), StorageError>;
}

// Compile-time proof that `dyn StorageBackend` is object-safe. The registry's
// `Arc<dyn StorageBackend>` also requires this.
fn _assert_object_safe(_b: &dyn StorageBackend) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_range_checked_arithmetic() {
        assert!(ReadRange::new(0, 0).is_ok());
        assert!(ReadRange::new(u64::MAX, 1).is_err());
        let r = ReadRange::new(10, 5).unwrap();
        assert_eq!(r.offset(), 10);
        assert_eq!(r.length(), 5);
        assert_eq!(r.end(), 15);
        assert!(!r.is_empty());
        assert!(ReadRange::new(0, 0).unwrap().is_empty());
    }

    #[test]
    fn operation_context_defaults() {
        let ctx = OperationContext::none();
        assert!(!ctx.is_cancelled());
        assert!(ctx.deadline().is_none());
        assert!(!ctx.deadline_passed());
    }

    #[test]
    fn operation_context_cancel_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let ctx = OperationContext::with_cancel(flag.clone());
        assert!(!ctx.is_cancelled());
        flag.store(true, Ordering::Release);
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn combined_context_preserves_deadline_and_shared_cancellation() {
        let flag = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let ctx = OperationContext::with_deadline_and_cancel(deadline, flag.clone());
        assert_eq!(ctx.deadline(), Some(deadline));
        assert!(!ctx.deadline_passed());
        assert!(!ctx.is_cancelled());
        flag.store(true, Ordering::Release);
        assert!(ctx.is_cancelled());
        assert_eq!(ctx.deadline(), Some(deadline));
    }

    #[test]
    fn write_options_default_overwrites_without_version() {
        let opts = WriteOptions::default();
        assert!(opts.overwrite);
        assert!(opts.expected_version.is_none());
    }
}
