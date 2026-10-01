//! Administrative rclone calls: capture, recursive listing, config dump and crypt checks.

use super::*;

impl RcloneContext {
    pub(crate) fn capture(
        &self,
        ctx: &OperationContext,
        args: &[&str],
    ) -> Result<Vec<u8>, StorageError> {
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: ADMIN_LIMIT,
        };
        let address = args
            .iter()
            .position(|arg| *arg == "--")
            .and_then(|at| args.get(at + 1));
        let _permit = match address {
            Some(address) => permit(ctx, address)?,
            None => None,
        };
        let op = address.map(|address| self.op(address, traffic::Direction::Other));
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut command = match address {
            Some(address) => self.command_for(address, &args),
            None => self.command(&args),
        };
        let result = process::run_metered(&mut command, ctx, None, &mut sink, false, op.as_ref());
        if let Some(op) = op {
            op.finish(&result);
        }
        result?;
        Ok(sink.bytes)
    }
    /// Recursive `lsjson` of a user tree, which may be far larger than admin
    /// output. Still bounded so a runaway listing cannot exhaust memory.
    pub(crate) fn list_recursive(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<Vec<u8>, StorageError> {
        if let Some(daemon) = self.daemon_for(address) {
            let _permit = permit(ctx, address)?;
            let op = self.op(address, traffic::Direction::Other);
            match daemon.list(ctx, address, LIST_LIMIT) {
                Ok(bytes) => {
                    op.received(bytes.len() as u64);
                    op.finish(&Ok::<(), StorageError>(()));
                    return Ok(bytes);
                }
                Err(daemon::Failure::Definite(error)) => {
                    let result = Err(error);
                    op.finish(&result);
                    return result;
                }
                Err(daemon::Failure::Fallback) => {}
            }
        }
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: LIST_LIMIT,
        };
        let args = ["lsjson", "-R", "--no-mimetype", "--", address];
        let _permit = permit(ctx, address)?;
        let op = self.op(address, traffic::Direction::Other);
        let result = process::run_metered(
            &mut self.command_for(
                address,
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
            ),
            ctx,
            None,
            &mut sink,
            false,
            Some(&op),
        );
        op.finish(&result);
        result?;
        Ok(sink.bytes)
    }
    pub(crate) fn config_dump(&self, ctx: &OperationContext) -> Result<Value, StorageError> {
        let bytes = self.capture(ctx, &["config", "dump"])?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid rclone config JSON"))?;
        if !value.is_object() {
            return Err(invalid("rclone config must be an object"));
        }
        Ok(value)
    }
    pub(crate) fn ensure_crypt(
        &self,
        ctx: &OperationContext,
        destination: &str,
    ) -> Result<(), StorageError> {
        process::check(ctx)?;
        let remote = remote_name(destination)?;
        self.check_policy_environment(false)?;
        let config = self.config_dump(ctx)?;
        let entry = config
            .get(remote)
            .ok_or_else(|| invalid("rclone destination remote is not configured"))?;
        if !entry
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("crypt"))
        {
            return Err(invalid("refusing write to non-crypt destination"));
        }
        match entry.get("no_data_encryption") {
            None | Some(Value::Bool(false)) => Ok(()),
            Some(Value::String(text))
                if matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "no" | "off"
                ) =>
            {
                Ok(())
            }
            Some(Value::Bool(true)) => Err(invalid("crypt data encryption disabled")),
            Some(Value::String(text))
                if matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                ) =>
            {
                Err(invalid("crypt data encryption disabled"))
            }
            _ => Err(invalid("invalid crypt encryption setting")),
        }
    }
    /// Freeze environment for inspection/I/O and fail closed on overrides that
    /// could make effective policy disagree with config dump. Never log values.
    /// Native crypt also refuses every `RCLONE_CRYPT_*` override, because RPool
    /// encrypts from the config dump alone.
    pub(crate) fn check_policy_environment(&self, native_crypt: bool) -> Result<(), StorageError> {
        if self.environment.iter().any(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            key == "RCLONE_CRYPT_NO_DATA_ENCRYPTION"
                || (native_crypt && key.starts_with("RCLONE_CRYPT_"))
                || (key.starts_with("RCLONE_CONFIG_") && key != "RCLONE_CONFIG_PASS")
        }) {
            return Err(invalid(
                "rclone policy-changing environment override is not supported",
            ));
        }
        Ok(())
    }
}
