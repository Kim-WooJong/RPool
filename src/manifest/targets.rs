use crate::models::Manifest;
use std::collections::BTreeSet;

pub(crate) fn manifest_remotes(manifest: &Manifest) -> Vec<String> {
    let mut remotes = BTreeSet::new();
    for shard in &manifest.shards {
        remotes.insert(shard.remote.clone());
    }
    remotes.into_iter().collect()
}
