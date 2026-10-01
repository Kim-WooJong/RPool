//! Object stat and ranged/full reads through rclone.

use super::*;

impl RcloneContext {
    /// `lsjson --stat [--hash --hash-type ...]` output for one address,
    /// through the daemon when it gives a definite answer.
    pub(super) fn stat_json(
        &self,
        ctx: &OperationContext,
        address: &str,
        hashes: &[String],
    ) -> Result<Vec<u8>, StorageError> {
        if let Some(daemon) = self.daemon_for(address) {
            let opt = if hashes.is_empty() {
                json!({})
            } else {
                json!({"showHash": true, "hashTypes": hashes})
            };
            let _permit = permit(ctx, address)?;
            let op = self.op(address, traffic::Direction::Other);
            let answered = match daemon.stat(ctx, address, opt) {
                Ok(item) => {
                    Some(serde_json::to_vec(&item).map_err(|_| invalid("invalid rclone stat JSON")))
                }
                Err(daemon::Failure::Definite(error)) => Some(Err(error)),
                Err(daemon::Failure::Fallback) => None,
            };
            if let Some(result) = answered {
                if let Ok(bytes) = &result {
                    op.received(bytes.len() as u64);
                }
                op.finish(&result);
                return result;
            }
        }
        let mut args = vec!["lsjson", "--stat"];
        if !hashes.is_empty() {
            args.push("--hash");
            for hash in hashes {
                args.extend(["--hash-type", hash.as_str()]);
            }
        }
        args.extend(["--", address]);
        self.capture(ctx, &args)
    }
    pub(crate) fn stat_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<ObjectMetadata, StorageError> {
        let bytes = self.stat_json(ctx, address, &[])?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid rclone stat JSON"))?;
        let is_dir = value
            .get("IsDir")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid("missing stat IsDir"))?;
        let size = if is_dir {
            0
        } else {
            value
                .get("Size")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("missing stat Size"))?
        };
        // Objects only: a directory's time moves whenever its children change.
        let modified = (!is_dir)
            .then(|| value.get("ModTime").and_then(Value::as_str))
            .flatten()
            .map(str::to_owned);
        Ok(ObjectMetadata {
            size,
            is_dir,
            version: None,
            modified,
        })
    }
    pub(crate) fn read_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        range: Option<&ReadRange>,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        if let Some(range) = range {
            if range.is_empty() {
                if self.stat_raw(ctx, address)?.is_dir {
                    return Err(invalid("cannot read a directory"));
                }
                return Ok(ReadReceipt {
                    bytes_read: 0,
                    version: None,
                });
            }
            // rclone CLI offset/count are signed int64. Reject rather than wrap.
            if range.offset() > i64::MAX as u64 || range.length() > i64::MAX as u64 {
                return Err(invalid("rclone range exceeds signed 64-bit CLI limits"));
            }
        }
        let (offset, count) = range.map_or((0, None), |r| (r.offset(), Some(r.length())));
        let mut bounded = RangeSink {
            sink,
            remaining: count.unwrap_or(u64::MAX),
        };
        let mut written = 0u64;
        let mut op = None;
        if let Some(daemon) = self.daemon_for(address) {
            let outcome = {
                let _permit = permit(ctx, address)?;
                let op = op.insert(self.op(address, traffic::Direction::Download));
                let mut metered = traffic::Metered {
                    sink: &mut bounded,
                    op,
                    ctx,
                };
                daemon.read(ctx, address, offset, count, &mut metered, &mut written)
            };
            match outcome {
                Ok(()) => {
                    let result = Ok(ReadReceipt {
                        bytes_read: written,
                        version: None,
                    });
                    if let Some(op) = op {
                        op.finish(&result);
                    }
                    return result;
                }
                Err(daemon::Failure::Definite(error)) => {
                    let result = Err(error);
                    if let Some(op) = op {
                        op.finish(&result);
                    }
                    return result;
                }
                Err(daemon::Failure::Fallback) => {}
            }
        }
        // Resume after whatever the daemon already delivered.
        let count = count.map(|count| count - written);
        if count == Some(0) {
            let result = Ok(ReadReceipt {
                bytes_read: written,
                version: None,
            });
            if let Some(op) = op {
                op.finish(&result);
            }
            return result;
        }
        let mut args = vec![OsString::from("cat")];
        if range.is_some() || written > 0 {
            args.extend(["--offset".into(), (offset + written).to_string().into()]);
        }
        if let Some(count) = count {
            args.extend(["--count".into(), count.to_string().into()]);
        }
        args.extend(["--".into(), address.into()]);
        let _permit = permit(ctx, address)?;
        let op = op.unwrap_or_else(|| self.op(address, traffic::Direction::Download));
        let result = process::run_metered(
            &mut self.command_for(address, &args),
            ctx,
            None,
            &mut bounded,
            false,
            Some(&op),
        )
        .map(|bytes_read| ReadReceipt {
            bytes_read: written + bytes_read,
            version: None,
        });
        op.finish(&result);
        result
    }
    pub(crate) fn read_all_raw(
        &self,
        ctx: &OperationContext,
        address: &str,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let cap = limit.unwrap_or(usize::MAX);
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: cap,
        };
        let range = limit.map(|n| ReadRange::new(0, n as u64)).transpose()?;
        self.read_raw(ctx, address, range.as_ref(), &mut sink)?;
        Ok(sink.bytes)
    }
    /// Starts the shared read daemon now (when allowed), so a later timed
    /// read does not include the daemon's own start-up.
    pub(crate) fn warm_read_daemon(&self) {
        let _ = daemon::get(self);
    }
}
