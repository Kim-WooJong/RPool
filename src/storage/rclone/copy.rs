//! Server-side copies, backend feature probing and crypt base/hash lookups.

use super::*;

impl RcloneContext {
    /// Copies `source` to `destination` with `rclone copyto --retries 1` after the
    /// crypt write gate, creating the parent dir first; retries go through
    /// `retry_rejected`. Unlike `copy_object`, it does not refuse an existing
    /// destination.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Native copy implements the retained same-backend copy contract"
        )
    )]
    pub(crate) fn copy_raw(
        &self,
        ctx: &OperationContext,
        source: &str,
        destination: &str,
    ) -> Result<(), StorageError> {
        self.ensure_crypt(ctx, destination)?;
        let args = [
            "copyto",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            source,
            destination,
        ]
        .map(OsString::from);
        self.ensure_parent_dir(ctx, destination);
        self.retry_rejected(ctx, destination, &args, traffic::Direction::Upload)
    }
    /// Copies one object from `source` to `destination` with `rclone copyto`,
    /// never overwriting: an existing destination is refused before the copy,
    /// and `--ignore-existing` closes the race window (a destination that
    /// appears in between is left alone and fails the caller's verification).
    /// The destination must pass the crypt write gate (`ensure_crypt`).
    ///
    /// - Same rclone remote name (e.g. `c1:old/x` -> `c1:new/x`): rclone uses
    ///   the backend's server-side copy when it has one (`Features.Copy`; a
    ///   crypt remote has it when its base has it). A crypt server-side copy
    ///   copies the base object verbatim under the encrypted destination name,
    ///   so no data passes through this PC and the ciphertext stays valid (the
    ///   file nonce lives in the object header; the data key is the remote's).
    ///   Without server-side copy rclone falls back to a streamed copy.
    /// - Different remotes: rclone streams the object through this PC
    ///   (decrypting and re-encrypting for crypt remotes); no temp file.
    pub(crate) fn copy_object(
        &self,
        ctx: &OperationContext,
        source: &str,
        destination: &str,
    ) -> Result<(), StorageError> {
        remote_name(source)?;
        self.ensure_crypt(ctx, destination)?;
        // Bucket-based backends stat a missing key as a directory; copyto
        // refuses to replace a real directory by itself.
        match self.stat_raw(ctx, destination) {
            Ok(metadata) if !metadata.is_dir => {
                return Err(StorageError::AlreadyExists {
                    path: "copy destination".into(),
                })
            }
            Ok(_) => {}
            Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let args = [
            "copyto",
            "--ignore-existing",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
            "--",
            source,
            destination,
        ]
        .map(OsString::from);
        self.ensure_parent_dir(ctx, destination);
        self.retry_rejected(ctx, destination, &args, traffic::Direction::Upload)
    }
    /// `rclone backend features <remote>`: server-side copy support and hashes.
    pub(crate) fn backend_features(
        &self,
        ctx: &OperationContext,
        remote: &str,
    ) -> Result<BackendFeatures, StorageError> {
        let bytes = self.capture(ctx, &["backend", "features", "--", remote])?;
        parse_backend_features(&bytes)
    }
    /// What a copy inside the crypt remote of `address` can do: whether its
    /// backend copies server-side and which hash (if any) the crypt's base
    /// reports for the ciphertext objects. Refuses non-crypt remotes.
    pub(crate) fn crypt_copy_capabilities(
        &self,
        ctx: &OperationContext,
        address: &str,
    ) -> Result<CryptCopyCapabilities, StorageError> {
        let name = remote_name(address)?;
        let config = self.config_dump(ctx)?;
        let entry = config
            .get(name)
            .ok_or_else(|| invalid("rclone remote is not configured"))?;
        if !entry
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("crypt"))
        {
            return Err(invalid("not a crypt remote"));
        }
        let base = entry
            .get("remote")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| invalid("crypt remote has no base"))?
            .to_owned();
        let own = self.backend_features(ctx, &format!("{name}:"))?;
        let below = self.backend_features(ctx, &base)?;
        Ok(CryptCopyCapabilities {
            server_side_copy: own.copy,
            hashes: preferred_hashes(&below.hashes),
            base,
        })
    }
    /// Address of the ciphertext object behind a crypt `address` on the crypt's
    /// `base` (from `crypt_copy_capabilities`). rclone encrypts the name
    /// (`cryptdecode --reverse`), so every name-encryption option is honoured.
    pub(crate) fn crypt_base_object(
        &self,
        ctx: &OperationContext,
        address: &str,
        base: &str,
    ) -> Result<String, StorageError> {
        let name = remote_name(address)?;
        let path = &address[name.len() + 1..];
        if path.is_empty() {
            return Err(invalid("crypt object path is empty"));
        }
        let remote = format!("{name}:");
        let bytes = self.capture(ctx, &["cryptdecode", "--reverse", "--", &remote, path])?;
        let encrypted = parse_cryptdecode(&bytes, path)?;
        Ok(join_base(base, &encrypted))
    }
    /// (size, `type:value`) of one plain object for the first of `hashes` it
    /// reports, or None when it reports none of them (backends may advertise
    /// a hash they do not return for every object).
    pub(crate) fn object_hash(
        &self,
        ctx: &OperationContext,
        address: &str,
        hashes: &[String],
    ) -> Result<Option<(u64, String)>, StorageError> {
        if hashes.is_empty() {
            return Ok(None);
        }
        let bytes = self.stat_json(ctx, address, hashes)?;
        parse_object_hash(&bytes, hashes)
    }
}
