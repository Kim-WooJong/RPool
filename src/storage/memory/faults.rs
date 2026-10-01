//! Test-only faults on the real StorageBackend boundary. No sleeps or random faults.
//! Call indices are per operation, one-based, and counted when the wrapper is entered.
//! Concurrent callers must use explicit synchronization to establish their order.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Barrier, Mutex};

use super::check_context;
use crate::storage::capabilities::BackendCapabilities;
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Operation {
    Stat,
    Read,
    ReadAll,
    Write,
    Delete,
    List,
    Copy,
    Rename,
}

/// Both barriers have two parties: test controller and operation thread.
#[derive(Clone)]
pub(crate) struct Gate {
    pub(crate) reached: Arc<Barrier>,
    pub(crate) release: Arc<Barrier>,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self {
            reached: Arc::new(Barrier::new(2)),
            release: Arc::new(Barrier::new(2)),
        }
    }
    fn wait(&self) {
        self.reached.wait();
        self.release.wait();
    }
}

pub(crate) enum Fault {
    CancelThenError {
        cancel: Arc<std::sync::atomic::AtomicBool>,
        error: StorageError,
    },
    /// Cancellation-aware read stall with a watchdog, unlike the legacy barrier.
    WaitForCancel {
        started: Arc<std::sync::atomic::AtomicBool>,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        late_write: bool,
    },
    /// Returned before delegation; storage is untouched.
    Error(StorageError),
    PauseBefore(Gate),
    /// Flip the first delivered byte; empty reads remain empty.
    CorruptRead,
    /// Deliver at most this many bytes and accurately report the short result.
    ShortRead(usize),
    /// Fail the source after N consumed bytes. Atomic MemoryBackend publishes nothing.
    PartialWrite(usize),
    /// Commit succeeds but acknowledgement is lost. Optional gate pauses after commit.
    LoseResponse(Option<Gate>),
}

pub(crate) struct Rule {
    pub(crate) operation: Operation,
    pub(crate) call: u64,
    pub(crate) fault: Fault,
}

struct Schedule {
    counts: BTreeMap<Operation, u64>,
    rules: BTreeMap<(Operation, u64), Fault>,
}

pub(crate) struct FaultBackend {
    inner: Arc<dyn StorageBackend>,
    schedule: Mutex<Schedule>,
    keyed_reads: Mutex<BTreeMap<ObjectKey, std::collections::VecDeque<Fault>>>,
}

impl FaultBackend {
    pub(crate) fn new(
        inner: Arc<dyn StorageBackend>,
        rules: Vec<Rule>,
    ) -> Result<Self, StorageError> {
        let mut schedule = Schedule {
            counts: BTreeMap::new(),
            rules: BTreeMap::new(),
        };
        for rule in rules {
            let valid = match &rule.fault {
                Fault::CorruptRead | Fault::ShortRead(_) => {
                    matches!(rule.operation, Operation::Read | Operation::ReadAll)
                }
                Fault::PartialWrite(_) => rule.operation == Operation::Write,
                Fault::LoseResponse(_) => {
                    matches!(rule.operation, Operation::Write | Operation::Delete)
                }
                _ => true,
            };
            if !valid
                || rule.call == 0
                || schedule
                    .rules
                    .insert((rule.operation, rule.call), rule.fault)
                    .is_some()
            {
                return Err(StorageError::invalid_input(
                    "invalid or duplicate fault rule",
                ));
            }
        }
        Ok(Self {
            inner,
            schedule: Mutex::new(schedule),
            keyed_reads: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn keyed_reads(
        inner: Arc<dyn StorageBackend>,
        rules: Vec<(ObjectKey, Fault)>,
    ) -> Self {
        let backend = Self::new(inner, vec![]).unwrap();
        for (key, fault) in rules {
            backend
                .keyed_reads
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push_back(fault);
        }
        backend
    }

    fn enter(
        &self,
        operation: Operation,
        ctx: &OperationContext,
    ) -> Result<Option<Fault>, StorageError> {
        let fault = {
            let mut schedule = self.schedule.lock().map_err(|_| StorageError::Other {
                detail: "fault schedule lock poisoned".into(),
            })?;
            let count = schedule.counts.entry(operation).or_insert(0);
            *count = count.checked_add(1).ok_or_else(|| StorageError::Other {
                detail: "fault call counter exhausted".into(),
            })?;
            let call = *count;
            schedule.rules.remove(&(operation, call))
        };
        // No schedule/store lock may span a gate or user callback.
        check_context(ctx)?;
        match fault {
            Some(Fault::Error(error)) => Err(error),
            Some(Fault::PauseBefore(gate)) => {
                gate.wait();
                check_context(ctx)?;
                Ok(None)
            }
            other => Ok(other),
        }
    }

    fn after_commit(fault: Option<Fault>) -> Result<(), StorageError> {
        if let Some(Fault::LoseResponse(gate)) = fault {
            if let Some(gate) = gate {
                gate.wait();
            }
            return Err(StorageError::unknown_outcome(
                "injected lost acknowledgement after commit",
            ));
        }
        Ok(())
    }
}

struct ReadFilter<'a> {
    sink: &'a mut dyn Write,
    remaining: Option<usize>,
    corrupt: bool,
    delivered: u64,
}

impl Write for ReadFilter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.remaining.unwrap_or(bytes.len()).min(bytes.len());
        if n > 0 {
            if self.corrupt {
                self.sink.write_all(&[bytes[0] ^ 1])?;
                self.sink.write_all(&bytes[1..n])?;
                self.corrupt = false;
            } else {
                self.sink.write_all(&bytes[..n])?;
            }
        }
        self.delivered += n as u64;
        if let Some(left) = &mut self.remaining {
            *left -= n;
        }
        // Deliberately discard the suffix to emulate a short backend response.
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

struct FailingSource<'a> {
    source: &'a mut dyn Read,
    remaining: usize,
}
impl Read for FailingSource<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(io::Error::other("injected partial source failure"));
        }
        let count = buffer.len().min(self.remaining);
        let n = self.source.read(&mut buffer[..count])?;
        self.remaining -= n;
        Ok(n)
    }
}

impl StorageBackend for FaultBackend {
    fn id(&self) -> BackendId {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    fn stat(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.enter(Operation::Stat, ctx)?;
        self.inner.stat(ctx, key)
    }
    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let keyed = self
            .keyed_reads
            .lock()
            .unwrap()
            .get_mut(key)
            .and_then(|q| q.pop_front());
        let fault = if keyed.is_some() {
            keyed
        } else {
            self.enter(Operation::Read, ctx)?
        };
        match &fault {
            Some(Fault::CancelThenError { cancel, error }) => {
                cancel.store(true, std::sync::atomic::Ordering::Release);
                return Err(error.clone());
            }
            Some(Fault::Error(error)) => return Err(error.clone()),
            Some(Fault::PauseBefore(gate)) => gate.wait(),
            Some(Fault::WaitForCancel {
                started,
                stopped,
                late_write,
            }) => {
                use std::sync::atomic::Ordering;
                started.store(true, Ordering::Release);
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !ctx.is_cancelled()
                    && !ctx.deadline_passed()
                    && std::time::Instant::now() < until
                {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                stopped.store(true, Ordering::Release);
                if !ctx.is_cancelled() && !ctx.deadline_passed() {
                    return Err(StorageError::Other {
                        detail: "test stall watchdog fired".into(),
                    });
                }
                if *late_write {
                    return self.inner.read(&OperationContext::none(), key, range, sink);
                }
                return Err(StorageError::Cancelled {
                    detail: "test stalled read cancelled".into(),
                });
            }
            _ => {}
        }
        let remaining = match &fault {
            Some(Fault::ShortRead(n)) => Some(*n),
            _ => None,
        };
        let mut filter = ReadFilter {
            sink,
            remaining,
            corrupt: matches!(fault, Some(Fault::CorruptRead)),
            delivered: 0,
        };
        let mut receipt = self.inner.read(ctx, key, range, &mut filter)?;
        receipt.bytes_read = filter.delivered;
        Ok(receipt)
    }
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let fault = self.enter(Operation::ReadAll, ctx)?;
        let mut bytes = self.inner.read_all(ctx, key, limit)?;
        match fault {
            Some(Fault::ShortRead(n)) => bytes.truncate(n),
            Some(Fault::CorruptRead) => {
                if let Some(first) = bytes.first_mut() {
                    *first ^= 1;
                }
            }
            _ => (),
        }
        Ok(bytes)
    }
    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        let fault = self.enter(Operation::Write, ctx)?;
        if let Some(Fault::PartialWrite(n)) = &fault {
            return self.inner.write(
                ctx,
                key,
                &mut FailingSource {
                    source,
                    remaining: *n,
                },
                options,
            );
        }
        let receipt = self.inner.write(ctx, key, source, options)?;
        Self::after_commit(fault)?;
        Ok(receipt)
    }
    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        let fault = self.enter(Operation::Delete, ctx)?;
        self.inner.delete(ctx, key)?;
        Self::after_commit(fault)
    }
    fn list(
        &self,
        ctx: &OperationContext,
        prefix: &str,
        page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        self.enter(Operation::List, ctx)?;
        self.inner.list(ctx, prefix, page)
    }
    fn copy(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        self.enter(Operation::Copy, ctx)?;
        self.inner.copy(ctx, source, destination)
    }
    fn rename(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        self.enter(Operation::Rename, ctx)?;
        self.inner.rename(ctx, source, destination)
    }
}
