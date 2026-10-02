//! Remote root store ([`RemoteRootStore`]): default folder per rclone remote,
//! applied by `remote_root::apply_remote_root` to bare `name:` remotes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// The remote roots file; managed by the remote-root CLI commands (`remote_root::manage`).
pub(crate) struct RemoteRootStore {
    /// File format version (1).
    pub(crate) version: u32,
    #[serde(default)]
    /// rclone remote name -> root folder used when a remote is given as `name:`.
    pub(crate) roots: BTreeMap<String, String>,
}

impl Default for RemoteRootStore {
    fn default() -> Self {
        Self {
            version: 1,
            roots: BTreeMap::new(),
        }
    }
}
