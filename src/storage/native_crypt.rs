//! Native rclone-crypt storage backend (M2 of `docs/NATIVE_MOUNT_CRYPT_PLAN.md`).
//!
//! `CryptBackend` wraps the backend of a crypt remote's *base* remote. Keys and
//! bytes are encrypted exactly as rclone `crypt` would store them, so objects
//! stay interchangeable with the rclone crypt remote in both directions. Keys
//! passed in are plaintext logical keys; the inner backend only ever sees
//! encrypted names and ciphertext.
//!
//! Integrity: every block is authenticated. As with rclone, truncation exactly on
//! a block boundary is not detectable here; callers verify sizes and hashes.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "native crypt routing (M2b) not wired yet")
)]

use std::io::{self, Read, Write};
use std::sync::Arc;

use crate::crypt::cipher::Cipher;
use crate::crypt::data::{self, Nonce, BLOCK_DATA, BLOCK_SIZE, BLOCK_TAG, HEADER_SIZE};
use crate::storage::capabilities::BackendCapabilities;
use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectKey};
use crate::storage::traits::*;

pub(crate) mod route;

pub(crate) struct CryptBackend {
    id: BackendId,
    inner: Arc<dyn StorageBackend>,
    cipher: Arc<Cipher>,
}

fn corrupt(found: impl Into<String>) -> StorageError {
    StorageError::CorruptData {
        found: found.into(),
        expected: "rclone crypt object".into(),
    }
}

impl CryptBackend {
    pub(crate) fn new(id: BackendId, inner: Arc<dyn StorageBackend>, cipher: Arc<Cipher>) -> Self {
        Self { id, inner, cipher }
    }

    fn file_key(&self, key: &ObjectKey) -> Result<ObjectKey, StorageError> {
        let name = self
            .cipher
            .encrypt_file_name(key.as_str())
            .map_err(|_| StorageError::invalid_input("object key cannot be encrypted"))?;
        ObjectKey::new(name)
    }

    fn plain_size(&self, encrypted: u64) -> Result<u64, StorageError> {
        data::decrypted_size(encrypted).map_err(|_| corrupt(format!("encrypted size {encrypted}")))
    }

    fn header(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<(Nonce, Option<String>), StorageError> {
        let mut header = Vec::with_capacity(HEADER_SIZE as usize);
        let receipt = self
            .inner
            .read(ctx, key, &ReadRange::new(0, HEADER_SIZE)?, &mut header)?;
        let nonce = data::parse_header(&header).map_err(|_| corrupt("bad crypt header"))?;
        Ok((nonce, receipt.version))
    }
}

/// Decrypts ciphertext streamed by the inner backend, one block at a time, and
/// forwards the requested plaintext window to the caller's sink.
struct DecryptingSink<'a> {
    cipher: &'a Cipher,
    nonce: Nonce,
    pending: Vec<u8>,
    discard: u64,
    remaining: u64,
    written: u64,
    sink: &'a mut dyn Write,
    failure: Option<StorageError>,
}

impl DecryptingSink<'_> {
    fn emit(&mut self, sealed_len: usize) -> io::Result<()> {
        let plain = match self
            .cipher
            .open_block(&self.nonce, &self.pending[..sealed_len])
        {
            Ok(plain) => plain,
            Err(_) => {
                self.failure = Some(corrupt("crypt block failed authentication"));
                return Err(io::Error::new(io::ErrorKind::InvalidData, "crypt block"));
            }
        };
        self.pending.drain(..sealed_len);
        self.nonce.increment();
        let skip = (self.discard as usize).min(plain.len());
        self.discard -= skip as u64;
        let take = ((plain.len() - skip) as u64).min(self.remaining) as usize;
        if let Err(error) = self.sink.write_all(&plain[skip..skip + take]) {
            self.failure = Some(StorageError::TransientIo {
                detail: "read sink failed".into(),
            });
            return Err(error);
        }
        self.remaining -= take as u64;
        self.written += take as u64;
        Ok(())
    }

    fn finish(mut self) -> Result<u64, StorageError> {
        match self.pending.len() {
            0 => {}
            n if n <= BLOCK_TAG as usize => return Err(corrupt("truncated crypt block")),
            n => self
                .emit(n)
                .map_err(|_| self.failure.take().expect("failure recorded"))?,
        }
        Ok(self.written)
    }
}

impl Write for DecryptingSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while self.pending.len() >= BLOCK_SIZE as usize {
            self.emit(BLOCK_SIZE as usize)?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StorageBackend for CryptBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        let mut capabilities = self.inner.capabilities();
        // A provider object limit applies to the ciphertext.
        capabilities.max_object_size = capabilities
            .max_object_size
            .map(|limit| data::decrypted_size(limit).unwrap_or(0));
        capabilities
    }

    fn stat(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        let mut metadata = self.inner.stat(ctx, &self.file_key(key)?)?;
        if !metadata.is_dir {
            metadata.size = self.plain_size(metadata.size)?;
        }
        Ok(metadata)
    }

    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let key = self.file_key(key)?;
        let (nonce, header_version) = self.header(ctx, &key)?;
        if range.is_empty() {
            return Ok(ReadReceipt {
                bytes_read: 0,
                version: header_version,
            });
        }
        let start = data::range_start(range.offset());
        let last_block = (range.offset() + (range.length() - 1)) / BLOCK_DATA;
        let blocks = last_block - start.block + 1;
        let encrypted = ReadRange::new(
            start.encrypted_offset,
            blocks
                .saturating_mul(BLOCK_SIZE)
                .min(u64::MAX - start.encrypted_offset),
        )?;
        let mut first = nonce;
        first.add(start.block);
        let mut decrypting = DecryptingSink {
            cipher: &self.cipher,
            nonce: first,
            pending: Vec::with_capacity(BLOCK_SIZE as usize),
            discard: start.discard,
            remaining: range.length(),
            written: 0,
            sink,
            failure: None,
        };
        let receipt = match self.inner.read(ctx, &key, &encrypted, &mut decrypting) {
            Ok(receipt) => receipt,
            Err(error) => return Err(decrypting.failure.take().unwrap_or(error)),
        };
        if let (Some(a), Some(b)) = (&header_version, &receipt.version) {
            if a != b {
                return Err(StorageError::TransientIo {
                    detail: "crypt object changed during read".into(),
                });
            }
        }
        let bytes_read = decrypting.finish()?;
        Ok(ReadReceipt {
            bytes_read,
            version: receipt.version,
        })
    }

    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        let length = limit.map_or(u64::MAX - HEADER_SIZE, |limit| limit as u64);
        let mut out = Vec::new();
        self.read(ctx, key, &ReadRange::new(0, length)?, &mut out)?;
        Ok(out)
    }

    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        let key = self.file_key(key)?;
        let mut encrypting = self
            .cipher
            .encrypter(source)
            .map_err(|_| StorageError::Other {
                detail: "crypt nonce generation failed".into(),
            })?;
        let mut receipt = self.inner.write(ctx, &key, &mut encrypting, options)?;
        // The object may already be published, so a bad size is not a clean failure.
        receipt.size = self.plain_size(receipt.size).map_err(|_| {
            StorageError::unknown_outcome("crypt write receipt size is not a ciphertext size")
        })?;
        Ok(receipt)
    }

    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        self.inner.delete(ctx, &self.file_key(key)?)
    }

    fn list(
        &self,
        ctx: &OperationContext,
        prefix: &str,
        page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        // Encrypted names only preserve whole path segments.
        let encrypted_prefix = match prefix.strip_suffix('/') {
            None if prefix.is_empty() => String::new(),
            None => {
                return Err(StorageError::unsupported(
                    "crypt listing of a partial name prefix",
                ))
            }
            Some(directory) => format!(
                "{}/",
                self.cipher
                    .encrypt_dir_name(directory)
                    .map_err(|_| StorageError::invalid_input("list prefix cannot be encrypted"))?
            ),
        };
        let listed = self.inner.list(ctx, &encrypted_prefix, page)?;
        let mut entries = Vec::with_capacity(listed.entries.len());
        for entry in listed.entries {
            // Like rclone, names that do not decrypt are not part of the crypt view.
            let name = if entry.is_dir {
                self.cipher.decrypt_dir_name(entry.key.as_str())
            } else {
                self.cipher.decrypt_file_name(entry.key.as_str())
            };
            let Ok(Ok(key)) = name.map(ObjectKey::new) else {
                continue;
            };
            let size = if entry.is_dir {
                entry.size
            } else {
                self.plain_size(entry.size)?
            };
            entries.push(ListEntry {
                key,
                size,
                is_dir: entry.is_dir,
            });
        }
        Ok(ListPage {
            entries,
            next_page: listed.next_page,
        })
    }

    fn copy(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        // The stored header carries the nonce, so a byte copy stays decryptable.
        let mut receipt =
            self.inner
                .copy(ctx, &self.file_key(source)?, &self.file_key(destination)?)?;
        receipt.size = self.plain_size(receipt.size)?;
        Ok(receipt)
    }

    fn rename(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        self.inner
            .rename(ctx, &self.file_key(source)?, &self.file_key(destination)?)
    }
}

#[cfg(test)]
mod route_tests;
#[cfg(test)]
mod tests;
