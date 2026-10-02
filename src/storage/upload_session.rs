//! One archive upload through a `StorageWriter` (`put`): fewer provider
//! calls per shard without weaker checks.
//!
//! - A first attempt at a fresh archive skips the "already stored?" probe
//!   before each write: nothing can exist under its new id yet. A resumed
//!   upload (its journal existed) keeps probing and reusing verified shards.
//! - Native-crypt writes defer the provider-hash check; `finish` then checks
//!   the whole archive with one recursive listing per account. Shards the
//!   listing cannot prove are read back in full; what still fails is returned
//!   so the caller can re-upload it.
use super::stored_hash::Expected;
use super::writer::StorageWriter;
use crate::prelude::*;

#[derive(Debug, Default)]
/// State of one archive upload, stored in `StorageWriter::session` between
/// [`StorageWriter::begin_upload_session`] and [`StorageWriter::finish_upload_session`].
pub(crate) struct UploadSession {
    /// No earlier attempt exists, so writes skip the "already stored?" probe.
    fresh: bool,
    /// Shards whose provider-hash check was deferred, with what the provider must report.
    pending: Vec<(Shard, Expected)>,
}

impl StorageWriter {
    /// Starts an archive upload; `fresh` when no earlier attempt exists.
    pub(crate) fn begin_upload_session(&self, fresh: bool) {
        if let Ok(mut session) = self.session.lock() {
            *session = Some(UploadSession {
                fresh,
                pending: Vec::new(),
            });
        }
    }

    /// (skip the pre-write probe, defer the hash check) for the next write.
    pub(super) fn session_mode(&self) -> (bool, bool) {
        match self.session.lock().ok().as_deref() {
            Some(Some(session)) => (session.fresh, true),
            _ => (false, false),
        }
    }

    /// Queues `shard` for the archive-wide hash check in `finish_upload_session`
    /// (ignored outside a session). Called by `write_file` for deferred writes.
    pub(super) fn defer_check(&self, shard: Shard, expected: Expected) {
        if let Ok(mut session) = self.session.lock() {
            if let Some(session) = session.as_mut() {
                session.pending.push((shard, expected));
            }
        }
    }

    /// Ends the session: checks every deferred shard. Returns the shards that
    /// neither the provider hash nor a full readback could prove.
    pub(crate) fn finish_upload_session(&self) -> Vec<Shard> {
        let pending = match self.session.lock() {
            Ok(mut session) => session.take().map(|s| s.pending).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if pending.is_empty() {
            return Vec::new();
        }
        let expected: Vec<Expected> = pending.iter().map(|(_, e)| e.clone()).collect();
        let proven = match self.reader().legacy_context() {
            Some(context) => {
                context.check_stored_hashes(self.reader().operation_context(), &expected)
            }
            None => vec![false; pending.len()],
        };
        let mut failed = Vec::new();
        for ((shard, _), proven) in pending.into_iter().zip(proven) {
            if proven || self.reader().verify(&shard, true).is_ok() {
                super::rclone::traffic::credit_verified(&shard.object, shard.size);
            } else {
                self.reader().forget_verified(&shard.object);
                failed.push(shard);
            }
        }
        failed
    }
}
