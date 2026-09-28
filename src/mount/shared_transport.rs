//! Content-addressed shared events. No mutable remote catalog is published.
use crate::storage::{
    error::{StorageError, StorageErrorKind},
    rclone::RcloneContext,
    traits::OperationContext,
    writer::StorageWriter,
};
use crate::utils::remote_join;
use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

const EVENT_LIMIT: usize = 8 * 1024 * 1024;
const EVENT_COUNT_LIMIT: usize = 10_000;
const TOTAL_LIMIT: usize = 64 * 1024 * 1024;

pub(crate) struct SharedTransport {
    rclone: String,
    root: String,
}

#[derive(Deserialize)]
struct Listed {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Size")]
    size: i64,
    #[serde(rename = "IsDir")]
    is_dir: bool,
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_event(id: &str, bytes: &[u8]) -> Result<()> {
    if !valid_id(id) || bytes.len() > EVENT_LIMIT || blake3::hash(bytes).to_hex().as_str() != id {
        bail!("invalid shared event identity, hash, or size");
    }
    Ok(())
}
fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::NotFound)
}

impl SharedTransport {
    // Syntax only: runtime operations separately enforce the encrypted remote policy.
    pub(crate) fn new(rclone: &str, root: &str) -> Result<Self> {
        let Some((remote, path)) = root.split_once(':') else {
            bail!("shared root must be remote:path");
        };
        if remote.is_empty()
            || remote.starts_with('-')
            || (remote.len() == 1
                && remote.as_bytes()[0].is_ascii_alphabetic()
                && path.starts_with('/'))
            || !remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-. ".contains(&b))
            || path.contains(':')
            || root.chars().any(char::is_control)
            || path.contains('\\')
            || path.split('/').any(|p| p == "." || p == "..")
        {
            bail!("invalid shared root");
        }
        Ok(Self {
            rclone: rclone.into(),
            root: root.trim_end_matches('/').into(),
        })
    }

    pub(crate) fn list(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let context = RcloneContext::inherited(&self.rclone);
        let operation = OperationContext::none();
        context.ensure_crypt(&operation, &self.root)?;
        let events = remote_join(&self.root, "events");
        // capture itself limits listing stdout to 8 MiB. Only typed absence is empty.
        let listing = match context.capture(&operation, &["lsjson", "--files-only", "--", &events])
        {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == StorageErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(error.into()),
        };
        let entries: Vec<Listed> = serde_json::from_slice(&listing)?;
        if entries.len() > EVENT_COUNT_LIMIT {
            bail!("shared event count exceeds limit");
        }
        let storage = StorageWriter::rclone(&self.rclone);
        let mut result = BTreeMap::new();
        let mut total = 0usize;
        for entry in entries {
            if entry.is_dir {
                continue;
            }
            let Some(id) = entry.path.strip_suffix(".json") else {
                continue;
            };
            if !valid_id(id) || entry.size < 0 || entry.size as u64 > EVENT_LIMIT as u64 {
                bail!("invalid shared event listing entry");
            }
            total = total
                .checked_add(entry.size as usize)
                .ok_or_else(|| anyhow::anyhow!("shared event size overflow"))?;
            if total > TOTAL_LIMIT {
                bail!("shared event collection exceeds size limit");
            }
            let address = remote_join(&events, &entry.path);
            let metadata = storage.reader().stat(&address)?;
            if metadata.size != entry.size as u64 {
                bail!("shared event changed during listing");
            }
            let bytes = storage.reader().read_metadata(&address)?;
            validate_event(id, &bytes)?;
            if result.insert(id.into(), bytes).is_some() {
                bail!("duplicate shared event listing");
            }
        }
        Ok(result)
    }

    pub(crate) fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        validate_event(id, bytes)?;
        let storage = StorageWriter::rclone(&self.rclone);
        storage.ensure_destination(&self.root)?;
        let address = remote_join(&self.root, &format!("events/{id}.json"));
        match storage.reader().stat(&address) {
            Ok(metadata) => {
                if metadata.size != bytes.len() as u64 {
                    bail!("existing shared event differs; refusing overwrite");
                }
                if storage.reader().read_metadata(&address)? != bytes {
                    bail!("existing shared event differs; refusing overwrite");
                }
                return Ok(());
            }
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
        // Cooperating concurrent writers can only publish identical bytes at this ID.
        // rclone offers no atomic create-if-absent; this is not protection against hostile writers.
        storage.write_bytes(&address, bytes, 1)?;
        if storage.reader().read_metadata(&address)? != bytes {
            bail!("shared event readback mismatch");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_validation_is_offline() {
        assert!(SharedTransport::new("nonexistent-rclone", "crypt:shared").is_ok());
        for root in [
            "/tmp/local",
            ":path",
            "C:/local",
            "remote:../bad",
            "remote:a/./b",
            "remote:a\\b",
            "remote:a\nb",
        ] {
            assert!(SharedTransport::new("unused", root).is_err(), "{root:?}");
        }
    }
    #[test]
    fn event_identity_requires_exact_content() {
        let bytes = b"event";
        let id = blake3::hash(bytes).to_hex().to_string();
        assert!(validate_event(&id, bytes).is_ok());
        assert!(validate_event(&id, b"different").is_err());
        assert!(validate_event("../event", bytes).is_err());
        assert!(validate_event(&id.to_uppercase(), bytes).is_err());
    }
}
