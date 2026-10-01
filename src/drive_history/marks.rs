//! Published "purged from trash" marks, shared by every PC of the pool.
//!
//! A mark is an immutable, content-addressed JSON record under
//! `<metadata root>/history/events/<blake3>.json` on every metadata replica
//! (the v6 `events-v6/<scope>` or v7 `snapshots-v7/<scope>` generation root).
//! Older RPool never lists `history/`, so marks are invisible to it; they do
//! not change any v6 event or v7 snapshot. Marks only hide trash entries and
//! end retention protection; they never delete data themselves.
use crate::prelude::*;

pub(crate) const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mark {
    pub format: u32,
    /// `purge` (the only kind of format 1).
    pub kind: String,
    /// Deletion revisions (trash entry ids) purged by this mark.
    pub ids: BTreeSet<String>,
    pub worker: String,
    pub unix: u64,
}
impl Mark {
    pub(crate) fn purge(ids: BTreeSet<String>, worker: &str, unix: u64) -> Self {
        Self {
            format: FORMAT,
            kind: "purge".into(),
            ids,
            worker: worker.into(),
            unix,
        }
    }
    pub(crate) fn encode(&self) -> Result<(String, Vec<u8>)> {
        let bytes = serde_json::to_vec(self)?;
        Ok((blake3::hash(&bytes).to_hex().to_string(), bytes))
    }
}

/// One metadata replica's mark directory.
pub(crate) trait MarkStore {
    /// Every mark: id -> verified bytes.
    fn all(&self) -> Result<BTreeMap<String, Vec<u8>>>;
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()>;
}

/// Union of purged ids over every replica. Unknown formats/kinds are ignored
/// (a newer RPool may add kinds); a record whose bytes do not hash to its id
/// is rejected by the transport.
pub(crate) fn purged(stores: &[&dyn MarkStore]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for store in stores {
        for bytes in store.all()?.values() {
            if let Ok(mark) = serde_json::from_slice::<Mark>(bytes) {
                if mark.format == FORMAT && mark.kind == "purge" {
                    ids.extend(mark.ids);
                }
            }
        }
    }
    Ok(ids)
}

/// Publishes `mark` to every replica (an error leaves a partial, retryable
/// publication: the same bytes get the same id).
pub(crate) fn publish(stores: &[&dyn MarkStore], mark: &Mark) -> Result<String> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    let (id, bytes) = mark.encode()?;
    for store in stores {
        store.publish(&id, &bytes)?;
    }
    Ok(id)
}

/// Real replicas: `SharedTransport` at `<root>/history`.
pub(crate) struct Remote(crate::mount::history_bridge::Transport);
impl Remote {
    pub(crate) fn open(rclone: &str, roots: &[String], native_crypt: bool) -> Result<Vec<Self>> {
        roots
            .iter()
            .map(|root| {
                crate::mount::history_bridge::Transport::new(
                    rclone,
                    &crate::utils::remote_join(root, "history"),
                )
                .map(|t| Self(t.with_native_crypt(native_crypt)))
            })
            .collect()
    }
}
impl MarkStore for Remote {
    fn all(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        self.0.list_missing(&BTreeSet::new())
    }
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        self.0.publish(id, bytes)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    pub(crate) struct Store(pub RefCell<BTreeMap<String, Vec<u8>>>);
    impl MarkStore for Store {
        fn all(&self) -> Result<BTreeMap<String, Vec<u8>>> {
            Ok(self.0.borrow().clone())
        }
        fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
            self.0.borrow_mut().insert(id.into(), bytes.to_vec());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Store;
    use super::*;

    #[test]
    fn marks_union_over_replicas_and_ignore_unknown_kinds() {
        let (a, b) = (Store::default(), Store::default());
        let stores: Vec<&dyn MarkStore> = vec![&a, &b];
        publish(&stores, &Mark::purge(["x".into()].into(), "pc", 1)).unwrap();
        b.publish(
            "other",
            br#"{"format":2,"kind":"purge","ids":["y"],"worker":"w","unix":1}"#,
        )
        .unwrap();
        let (id, bytes) = Mark::purge(["z".into()].into(), "pc", 2).encode().unwrap();
        b.publish(&id, &bytes).unwrap();
        assert_eq!(
            purged(&stores).unwrap(),
            ["x".to_string(), "z".into()].into()
        );
        assert_eq!(a.0.borrow().len(), 1);
        assert!(publish(&[], &Mark::purge(BTreeSet::new(), "pc", 1)).is_err());
    }
}
