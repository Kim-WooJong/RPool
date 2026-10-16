//! The storage manager's journal on the provider itself:
//! `<root>/.rpool-sync/storage-cleanup-v1/<blake3>.json`, immutable,
//! content-addressed records published through `SharedTransport` (the same
//! write-once semantics as pool-sync events and purge marks), so every PC
//! that manages this provider sees what is marked, being deleted and
//! deleted. The records are the drive cleanup's (`drive_history::cleanup::
//! records`), keyed by unit (`layout::UnitKind::key`); older RPool never
//! lists this folder.
use crate::drive_history::marks::MarkStore;
use crate::mount::history_bridge::Transport as SharedTransport;
use crate::prelude::*;

/// The journal folder of one provider root.
pub(crate) struct Store(SharedTransport);

impl Store {
    /// The journal of provider root `root` (`remote:path`).
    pub(crate) fn open(rclone: &str, root: &str) -> Result<Self> {
        let base = crate::utils::remote_join(root, ".rpool-sync");
        Ok(Self(
            SharedTransport::new(rclone, &base)?.in_dir("storage-cleanup-v1"),
        ))
    }
}

impl MarkStore for Store {
    fn all(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        self.0.list_missing(&BTreeSet::new())
    }
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        self.0.publish(id, bytes)
    }
}
