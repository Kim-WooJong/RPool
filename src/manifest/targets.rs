use crate::models::Manifest;
use std::collections::BTreeSet;

pub(crate) fn manifest_remotes(manifest: &Manifest) -> Vec<String> {
    let mut remotes = BTreeSet::new();
    for shard in &manifest.shards {
        remotes.insert(shard.remote.clone());
    }
    remotes.into_iter().collect()
}

/// Quota-aware modes skip exhausted targets for both shards and metadata.
/// Replicate metadata to every actually used target, not unused/full members.
/// Free-ratio is quota-aware too: a small file uses only some accounts, and
/// writing its manifest to the others (one write at a time on Dropbox) made
/// many small uploads queue behind each other for no recovery benefit.
pub(crate) fn publication_remotes(
    manifest: &Manifest,
    requested: &[String],
    placement: crate::models::Placement,
) -> Vec<String> {
    if matches!(
        placement,
        crate::models::Placement::Resilient
            | crate::models::Placement::FreeRatio
            | crate::models::Placement::Proportional
            | crate::models::Placement::CapacityFirst
    ) {
        manifest_remotes(manifest)
    } else {
        requested.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resilient_does_not_publish_metadata_to_unused_full_targets() {
        let manifest = Manifest {
            version: 1,
            archive_id: "test".into(),
            original_name: "file".into(),
            original_size: 1,
            shard_size: 1,
            created_unix: 0,
            content_root_blake3: String::new(),
            coding: None,
            shards: vec![crate::models::Shard {
                index: 0,
                offset: 0,
                size: 1,
                remote: "usable:".into(),
                object: "usable:test/shards/0".into(),
                blake3: String::new(),
                kind: crate::models::ShardKind::Data,
                group: 0,
                slot: 0,
            }],
        };
        let requested = vec!["full:".into(), "usable:".into()];
        assert_eq!(
            publication_remotes(&manifest, &requested, crate::models::Placement::Resilient),
            vec!["usable:"]
        );
        assert_eq!(
            publication_remotes(
                &manifest,
                &requested,
                crate::models::Placement::CapacityFirst
            ),
            vec!["usable:"]
        );
        assert_eq!(
            publication_remotes(&manifest, &requested, crate::models::Placement::FreeRatio),
            vec!["usable:"]
        );
        assert_eq!(
            publication_remotes(&manifest, &requested, crate::models::Placement::RoundRobin),
            requested
        );
    }
}
