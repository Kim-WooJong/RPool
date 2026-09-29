//! Content-addressed shared events. No mutable remote catalog is published.
use crate::storage::{
    error::{StorageError, StorageErrorKind},
    rclone::RcloneContext,
    traits::OperationContext,
    writer::StorageWriter,
};
use crate::utils::remote_join;
use anyhow::{bail, Context, Result};
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

    pub(crate) fn list_missing(
        &self,
        known: &std::collections::BTreeSet<String>,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let context = RcloneContext::inherited(&self.rclone);
        let operation = OperationContext::none();
        context.ensure_crypt(&operation, &self.root)?;
        let events = remote_join(&self.root, "events");
        let storage = StorageWriter::rclone(&self.rclone);
        let mut result = BTreeMap::new();
        let mut total = 0usize;
        let mut count = 0usize;
        // Most roots have a small event directory: one bounded listing avoids
        // 16 remote round trips. If it exceeds the existing 8 MiB output cap,
        // retain the original hash-prefix pages rather than relaxing that cap.
        let mut complete =
            match context.capture(&operation, &["lsjson", "--files-only", "--", &events]) {
                Ok(bytes) => Some(group_listing(serde_json::from_slice(&bytes)?)),
                Err(error) if error.kind() == StorageErrorKind::NotFound => {
                    Some(std::array::from_fn(|_| Vec::new()))
                }
                Err(StorageError::OutputBoundsViolated) => None,
                Err(error) => return Err(error.into()),
            };
        for (index, prefix) in b"0123456789abcdef".iter().copied().enumerate() {
            let entries = if let Some(buckets) = complete.as_mut() {
                std::mem::take(&mut buckets[index])
            } else {
                let filter = format!("{}*.json", prefix as char);
                let listing = match context.capture(
                    &operation,
                    &[
                        "lsjson",
                        "--files-only",
                        "--include",
                        &filter,
                        "--",
                        &events,
                    ],
                ) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == StorageErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                serde_json::from_slice(&listing)?
            };
            for (id, entry) in missing_entries(entries, known, prefix, &mut count, &mut total)? {
                let address = remote_join(&events, &entry.path);
                let metadata = storage.reader().stat(&address)?;
                if metadata.size != entry.size as u64 {
                    bail!("shared event changed during listing");
                }
                let bytes = storage.reader().read_metadata(&address)?;
                validate_event(&id, &bytes)?;
                if result.insert(id, bytes).is_some() {
                    bail!("duplicate shared event listing");
                }
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

/// Match the old `<hex>*.json` filters before applying their validation rules.
/// Other names were not returned by those filtered listings.
fn group_listing(entries: Vec<Listed>) -> [Vec<Listed>; 16] {
    let mut buckets = std::array::from_fn(|_| Vec::new());
    for entry in entries {
        if entry.path.ends_with(".json") {
            if let Some(index) = b"0123456789abcdef"
                .iter()
                .position(|prefix| entry.path.as_bytes().first() == Some(prefix))
            {
                buckets[index].push(entry);
            }
        }
    }
    buckets
}

fn missing_entries(
    entries: Vec<Listed>,
    known: &std::collections::BTreeSet<String>,
    prefix: u8,
    count: &mut usize,
    total: &mut usize,
) -> Result<Vec<(String, Listed)>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut missing = vec![];
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let id = entry
            .path
            .strip_suffix(".json")
            .context("unexpected shared event name")?;
        if !valid_id(id)
            || id.as_bytes()[0] != prefix
            || entry.size < 0
            || entry.size as u64 > EVENT_LIMIT as u64
            || !seen.insert(id.to_owned())
        {
            bail!("invalid, duplicate or cross-prefix shared event listing entry");
        }
        if known.contains(id) {
            continue;
        }
        *count = count
            .checked_add(1)
            .context("shared event count overflow")?;
        *total = total
            .checked_add(entry.size as usize)
            .context("shared event size overflow")?;
        if *count > EVENT_COUNT_LIMIT || *total > TOTAL_LIMIT {
            bail!("unseen shared history exceeds bounded bootstrap budget (10,000 events / 64 MiB). Existing known history is not charged; preserve workspace and use a coordinated new-root checkpoint before further growth");
        }
        missing.push((id.to_owned(), entry));
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_missing_uses_bounded_fast_path_and_only_overflow_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.rs");
        std::fs::write(&source, include_str!("shared_transport_fixture.rs.txt")).unwrap();
        let executable = dir.path().join(if cfg!(windows) {
            "fake rclone.exe"
        } else {
            "fake rclone"
        });
        let output =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg("--edition=2021")
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "fixture compilation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let scan = |root| {
            SharedTransport::new(executable.to_str().unwrap(), &format!("crypt:{root}"))
                .unwrap()
                .list_missing(&Default::default())
        };
        assert!(scan("small").unwrap().is_empty());
        assert!(scan("overflow").unwrap().is_empty());
        assert!(scan("auth")
            .unwrap_err()
            .to_string()
            .contains("authentication"));
        let calls = std::fs::read_to_string(executable.with_extension("calls")).unwrap();
        let calls: Vec<_> = calls.lines().collect();
        assert_eq!(calls.len(), 19);
        assert_eq!(&calls[..2], &["full", "full"]);
        assert!(calls[2..18].iter().all(|call| *call == "page"));
        assert_eq!(calls[18], "full");
    }
    #[test]
    fn complete_listing_preserves_prefix_filter_and_validation() {
        let make = |path: String| Listed {
            path,
            size: 1,
            is_dir: false,
        };
        let a = format!("{}.json", "a".repeat(64));
        let f = format!("{}.json", "f".repeat(64));
        let mut buckets = group_listing(vec![
            make(a.clone()),
            make(f.clone()),
            make("z.json".into()),
            make("a.txt".into()),
        ]);
        assert_eq!(buckets.iter().map(Vec::len).sum::<usize>(), 2);
        let known = ["a".repeat(64)].into_iter().collect();
        assert!(missing_entries(
            std::mem::take(&mut buckets[10]),
            &known,
            b'a',
            &mut 0,
            &mut 0
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            missing_entries(
                std::mem::take(&mut buckets[15]),
                &known,
                b'f',
                &mut 0,
                &mut 0
            )
            .unwrap()
            .len(),
            1
        );
        let mut duplicate = group_listing(vec![make(a.clone()), make(a)]);
        assert!(missing_entries(
            std::mem::take(&mut duplicate[10]),
            &Default::default(),
            b'a',
            &mut 0,
            &mut 0
        )
        .is_err());
        let mut invalid = group_listing(vec![make("a-not-an-id.json".into())]);
        assert!(missing_entries(
            std::mem::take(&mut invalid[10]),
            &Default::default(),
            b'a',
            &mut 0,
            &mut 0
        )
        .is_err());
    }
    #[test]
    fn known_history_does_not_consume_download_budget_but_entries_are_validated() {
        let known: std::collections::BTreeSet<_> =
            (0..10001).map(|i| format!("a{i:063x}")).collect();
        let entries = known
            .iter()
            .map(|id| Listed {
                path: format!("{id}.json"),
                size: 8192,
                is_dir: false,
            })
            .collect();
        let mut count = 0;
        let mut total = 0;
        assert!(
            missing_entries(entries, &known, b'a', &mut count, &mut total)
                .unwrap()
                .is_empty()
        );
        assert_eq!((count, total), (0, 0));
        let invalid = vec![Listed {
            path: format!("{}.json", "b".repeat(64)),
            size: 1,
            is_dir: false,
        }];
        assert!(missing_entries(invalid, &known, b'a', &mut count, &mut total).is_err());
    }
    #[test]
    fn unseen_bootstrap_remains_bounded_and_duplicates_rejected() {
        let entries = (0..10001)
            .map(|i| Listed {
                path: format!("a{i:063x}.json"),
                size: 1,
                is_dir: false,
            })
            .collect();
        assert!(missing_entries(entries, &Default::default(), b'a', &mut 0, &mut 0).is_err());
        let duplicate = (0..2)
            .map(|_| Listed {
                path: format!("{}.json", "a".repeat(64)),
                size: 1,
                is_dir: false,
            })
            .collect();
        assert!(missing_entries(duplicate, &Default::default(), b'a', &mut 0, &mut 0).is_err());
    }
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
