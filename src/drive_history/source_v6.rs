//! History of a v6 drive: one revision per namespace event (id = event id,
//! lineage = event path). Events are never deleted except by gated metadata
//! compaction, which keeps them inside checkpoints, so the whole history is
//! available (`metadata_pool::read_v6` without a workspace, or the
//! workspace's own events). Bytes are referenced again on restore; v6 has no
//! payload GC, so every content revision stays restorable.
use super::graph::{History, Rev, RevContent};
use crate::mount::history_bridge::Event;
use crate::prelude::*;

/// `times`: listing time per event; `unpublished` events count as `now`.
pub(crate) fn build(
    events: BTreeMap<String, Event>,
    times: &BTreeMap<String, u64>,
    unpublished: &BTreeSet<String>,
    now: u64,
    purged: BTreeSet<String>,
) -> Result<History> {
    let mut revs = BTreeMap::new();
    let mut payloads = BTreeMap::new();
    for (id, event) in &events {
        let time = times
            .get(id)
            .copied()
            .or_else(|| unpublished.contains(id).then_some(now));
        revs.insert(
            id.clone(),
            Rev {
                lineage: event.path.clone(),
                parents: event.parents.clone(),
                content: event.content.as_ref().map(|c| RevContent {
                    hash: c.hash.clone(),
                    size: c.size,
                }),
                author: event.worker.clone(),
                time,
            },
        );
        if let Some(content) = &event.content {
            payloads.insert(id.clone(), content.clone());
        }
    }
    History::new(revs, payloads, events, purged)
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Real v6 events (valid ids, manifests) for tests.
    use crate::mount::history_bridge::{Content, Event};
    use crate::prelude::*;
    pub(crate) fn event(worker: &str, path: &str, parents: &[&str], text: Option<&str>) -> Event {
        Event {
            version: 1,
            worker: worker.into(),
            device: format!("device-{worker}"),
            path: path.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            content: text.map(|text| {
                let size = text.len() as u64;
                let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                let shards = vec![Shard {
                    index: 0,
                    offset: 0,
                    size,
                    remote: "crypt:".into(),
                    object: format!("crypt:virtual-{hash}/data/0"),
                    blake3: hash.clone(),
                    kind: ShardKind::Data,
                    group: 0,
                    slot: 0,
                }];
                Content {
                    hash: hash.clone(),
                    size,
                    manifest: Manifest {
                        version: 2,
                        archive_id: format!("virtual-{hash}"),
                        original_name: path.rsplit('/').next().unwrap().into(),
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
                    },
                }
            }),
        }
    }
    /// Adds `event`, returns its id.
    pub(crate) fn add(events: &mut BTreeMap<String, Event>, event: Event) -> String {
        let id = event.id().unwrap();
        events.insert(id.clone(), event);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{add, event};
    use super::*;
    use crate::drive_history::model::{Retention, VersionKind};

    #[test]
    fn trash_and_versions_from_v6_events_including_conflict_copies() {
        let mut events = BTreeMap::new();
        let a1 = add(&mut events, event("PC-A", "Docs/a.txt", &[], Some("one")));
        let a2 = add(
            &mut events,
            event("PC-A", "Docs/a.txt", &[&a1], Some("two")),
        );
        let gone = add(&mut events, event("PC-B", "Docs/a.txt", &[&a2], None));
        let b1 = add(&mut events, event("PC-A", "b.txt", &[], Some("base")));
        let x = add(&mut events, event("PC-A", "b.txt", &[&b1], Some("x")));
        let y = add(&mut events, event("PC-B", "b.txt", &[&b1], Some("y")));
        let times: BTreeMap<_, _> = [(a1.clone(), 10), (a2.clone(), 20), (gone.clone(), 30)].into();
        let h = build(events, &times, &BTreeSet::new(), 100, BTreeSet::new()).unwrap();
        let trash = crate::drive_history::trash::list(&h, &Retention::default(), 100).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(
            (trash[0].id.as_str(), trash[0].path.as_str()),
            (gone.as_str(), "/Docs/a.txt")
        );
        assert_eq!((trash[0].deleted_unix, trash[0].size), (Some(30), 3));
        let versions = crate::drive_history::versions::list(&h, "Docs/a.txt").unwrap();
        let ids: Vec<_> = versions.iter().map(|v| v.id.clone()).collect();
        assert_eq!(ids, [gone, a2, a1]);
        assert_eq!(versions[0].kind, VersionKind::Deleted);
        assert!(versions.iter().all(|v| !v.current));
        // Conflict: original at the path, both branches as labelled copies.
        let view = h.view_at(None).unwrap();
        assert_eq!(view["b.txt"], b1);
        let copies: BTreeSet<_> = view.values().cloned().collect();
        assert!(copies.contains(&x) && copies.contains(&y));
        let versions = crate::drive_history::versions::list(&h, "b.txt").unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions.iter().filter(|v| v.current).count(), 3);
    }

    #[test]
    fn unpublished_events_are_newest_and_unknown_times_old() {
        let mut events = BTreeMap::new();
        let a1 = add(&mut events, event("pc", "f", &[], Some("1")));
        let a2 = add(&mut events, event("pc", "f", &[&a1], Some("2")));
        let h = build(
            events,
            &BTreeMap::new(),
            &[a2.clone()].into(),
            500,
            BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(h.revs[&a1].time, None);
        assert_eq!(h.revs[&a2].time, Some(500));
        assert_eq!(h.view_at(Some(100)).unwrap()["f"], a1);
    }
}
