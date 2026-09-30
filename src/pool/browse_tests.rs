use super::*;
use crate::models::{Manifest, Shard, ShardKind};
use crate::mount::pool_sync::EventStore;
use crate::mount::{TestContent, TestEvent};
use std::collections::{BTreeMap, BTreeSet};

/// Synthetic replica: records every publication so tests prove browse never writes.
#[derive(Default)]
struct Replica {
    events: BTreeMap<String, Vec<u8>>,
    publications: std::cell::Cell<usize>,
}
impl EventStore for Replica {
    fn missing(&self, known: &BTreeSet<String>) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
        Ok(self
            .events
            .iter()
            .filter(|(id, _)| !known.contains(*id))
            .map(|(id, b)| (id.clone(), b.clone()))
            .collect())
    }
    fn publish(&self, _: &str, _: &[u8]) -> anyhow::Result<()> {
        self.publications.set(self.publications.get() + 1);
        anyhow::bail!("browse must not publish")
    }
}
fn event(worker: &str, path: &str, parents: Vec<String>, size: Option<u64>) -> TestEvent {
    TestEvent {
        version: 1,
        worker: worker.into(),
        device: worker.into(),
        path: path.into(),
        parents,
        content: size.map(|size| {
            let bytes = vec![7u8; size as usize];
            let shards = vec![Shard {
                index: 0,
                offset: 0,
                size,
                remote: "crypt:".into(),
                object: format!("crypt:{path}/data/0"),
                blake3: blake3::hash(&bytes).to_hex().to_string(),
                kind: ShardKind::Data,
                group: 0,
                slot: 0,
            }];
            let manifest = Manifest {
                version: 2,
                archive_id: "archive".into(),
                original_name: "name".into(),
                original_size: size,
                shard_size: size.max(1),
                created_unix: 0,
                content_root_blake3: crate::manifest::content_root_v2(
                    size,
                    size.max(1),
                    &None,
                    &shards,
                ),
                coding: None,
                shards,
            };
            TestContent {
                hash: blake3::hash(&bytes).to_hex().to_string(),
                size,
                manifest,
            }
        }),
    }
}
fn add(replica: &mut Replica, event: TestEvent) -> String {
    let id = event.id().unwrap();
    replica
        .events
        .insert(id.clone(), serde_json::to_vec(&event).unwrap());
    id
}

#[test]
fn v6_listing_projects_files_implied_dirs_deletions_and_conflicts() {
    let mut r = Replica::default();
    add(&mut r, event("pc-a", "docs/nested/a.txt", vec![], Some(5)));
    let base = add(&mut r, event("pc-a", "top.bin", vec![], Some(3)));
    add(
        &mut r,
        event("pc-a", "top.bin", vec![base.clone()], Some(7)),
    );
    add(&mut r, event("pc-b", "top.bin", vec![base], Some(9)));
    let gone = add(&mut r, event("pc-a", "old/removed.txt", vec![], Some(1)));
    add(&mut r, event("pc-a", "old/removed.txt", vec![gone], None));

    let empty = Replica::default();
    assert!(project_v6(&[&empty]).unwrap().is_none());

    let (files, conflicts) = project_v6(&[&r, &empty]).unwrap().unwrap();
    let result = listing("p", "v6", files, &conflicts);
    assert_eq!(r.publications.get() + empty.publications.get(), 0);
    let paths: Vec<_> = result
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.is_dir, e.size))
        .collect();
    assert_eq!(paths[0], ("docs", true, 0));
    assert_eq!(paths[1], ("docs/nested", true, 0));
    assert_eq!(paths[2], ("docs/nested/a.txt", false, 5));
    assert!(paths.contains(&("top.bin", false, 3)), "{paths:?}");
    let copies: Vec<_> = paths
        .iter()
        .filter(|(p, _, _)| p.starts_with("top_") && p.ends_with(".bin"))
        .map(|(_, _, s)| *s)
        .collect();
    assert_eq!(copies.len(), 2, "{paths:?}");
    assert!(!paths.iter().any(|(p, _, _)| p.starts_with("old")));
    let mut sorted = result.entries.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(sorted, result.entries);
    assert!(result
        .notes
        .iter()
        .any(|n| n.starts_with("conflict at top.bin")));
    assert_eq!(result.mode, "v6");
}
