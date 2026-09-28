use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemoteRootStore {
    pub(crate) version: u32,
    #[serde(default)]
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
