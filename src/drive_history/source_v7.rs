//! History of a v7 drive: semantic revisions per stable file id, names from
//! the name records. A revision's id here is `<file id prefix>:<revision>`
//! (two files can share an identical first revision). Snapshots keep every
//! revision as causal evidence, but bytes only for the protocol's
//! `history_limit` closure; older bytes stay until the retired snapshot that
//! holds them is collected (deferred by retention, see
//! `mount::peer_snapshot::history`).
use super::graph::{History, Names, Payload, Projection, Rev, RevContent};
use crate::mount::history_bridge::Export;
use crate::prelude::*;

pub(crate) fn key(file: &str, revision: &str) -> String {
    format!("{}:{revision}", &file[..file.len().min(16)])
}

/// `(file id, revision id)` of a history revision id.
pub(crate) fn target(history: &History, key: &str) -> Result<(String, String)> {
    let rev = history
        .revs
        .get(key)
        .with_context(|| format!("unknown revision {key}"))?;
    let revision = key.rsplit_once(':').map_or(key, |(_, r)| r);
    Ok((rev.lineage.clone(), revision.to_owned()))
}

/// `times`: listing time per snapshot/name record; unpublished records count
/// as `now`.
pub(crate) fn build(
    data: &Export,
    times: &BTreeMap<String, u64>,
    now: u64,
    purged: BTreeSet<String>,
) -> Result<History> {
    let record_time = |id: &str, published: bool| {
        times
            .get(id)
            .copied()
            .or_else(|| (!published).then_some(now))
    };
    // Earliest snapshot holding a revision introduced it.
    let mut first: BTreeMap<(String, String), u64> = BTreeMap::new();
    for (id, snapshot) in &data.snapshots {
        let Some(time) = record_time(id, snapshot.published) else {
            continue;
        };
        for revision in &snapshot.revisions {
            first
                .entry((snapshot.file.clone(), revision.clone()))
                .and_modify(|t| *t = (*t).min(time))
                .or_insert(time);
        }
    }
    let mut revs = BTreeMap::new();
    let mut payloads = BTreeMap::new();
    for (file, revisions) in &data.revisions {
        let available = data.payloads.get(file);
        for (id, r) in revisions {
            let manifest = available.and_then(|p| p.get(id));
            revs.insert(
                key(file, id),
                Rev {
                    lineage: file.clone(),
                    parents: r.parents.iter().map(|p| key(file, p)).collect(),
                    content: r.content_hash.as_ref().map(|hash| RevContent {
                        hash: hash.clone(),
                        size: r.size,
                        restorable: manifest.is_some(),
                    }),
                    author: r.worker.clone(),
                    time: first.get(&(file.clone(), id.clone())).copied(),
                },
            );
            if let (Some(manifest), Some(_)) = (manifest, &r.content_hash) {
                payloads.insert(key(file, id), Payload::Manifest(manifest.clone()));
            }
        }
    }
    let purged = purged
        .into_iter()
        .filter(|id| revs.contains_key(id))
        .collect();
    History::new(
        "v7",
        revs,
        payloads,
        Projection::V7(names(data, times, now)),
        purged,
    )
}

/// Per file: `(time, path)` of its name records, oldest first (a record's
/// time never precedes its parents').
fn names(data: &Export, times: &BTreeMap<String, u64>, now: u64) -> Names {
    let mut effective: BTreeMap<&String, u64> = BTreeMap::new();
    let mut remaining: BTreeSet<&String> = data.names.keys().collect();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<&String> = remaining
            .iter()
            .filter(|id| {
                data.names[**id]
                    .parents
                    .values()
                    .flatten()
                    .all(|p| !remaining.contains(p))
            })
            .copied()
            .collect();
        if ready.is_empty() {
            break; // invalid ancestry is rejected by the snapshot runtime
        }
        for id in ready {
            let op = &data.names[id];
            let own = times
                .get(id)
                .copied()
                .or_else(|| (!op.published).then_some(now))
                .unwrap_or(0);
            let time = op
                .parents
                .values()
                .flatten()
                .filter_map(|p| effective.get(p))
                .fold(own, |a, b| a.max(*b));
            effective.insert(id, time);
            ordered.push(id);
            remaining.remove(id);
        }
    }
    ordered.sort_by_key(|id| effective[*id]);
    let mut result: Names = BTreeMap::new();
    for id in ordered {
        let time = Some(effective[id]).filter(|t| *t > 0);
        for (file, path) in &data.names[id].entries {
            result
                .entry(file.clone())
                .or_default()
                .push((time, path.clone()));
        }
    }
    result
}

#[cfg(test)]
pub(crate) mod fixture {
    use crate::mount::history_bridge::Export;
    use crate::mount::history_bridge::{ExportName, ExportRevision, ExportSnapshot};
    use crate::prelude::*;

    /// One file `file` with revisions `(id, parents, text or deletion)`, all
    /// introduced by one snapshot each at the given times, named `path`.
    pub(crate) fn export(
        file: &str,
        path: &str,
        revisions: &[(&str, &[&str], Option<&str>, u64)],
        bytes_kept: &[&str],
    ) -> (Export, BTreeMap<String, u64>) {
        let mut data = Export::default();
        let mut times = BTreeMap::new();
        let mut held = BTreeSet::new();
        for (n, (id, parents, text, time)) in revisions.iter().enumerate() {
            data.revisions.entry(file.into()).or_default().insert(
                id.to_string(),
                ExportRevision {
                    parents: parents.iter().map(|p| p.to_string()).collect(),
                    content_hash: text.map(|t| blake3::hash(t.as_bytes()).to_hex().to_string()),
                    size: text.map_or(0, |t| t.len() as u64),
                    worker: "pc".into(),
                },
            );
            held.insert(id.to_string());
            let snapshot = format!("s{n}");
            data.snapshots.insert(
                snapshot.clone(),
                ExportSnapshot {
                    file: file.into(),
                    revisions: held.clone(),
                    published: true,
                },
            );
            times.insert(snapshot, *time);
        }
        for id in bytes_kept {
            data.payloads.entry(file.into()).or_default().insert(
                id.to_string(),
                Manifest {
                    version: 2,
                    archive_id: "peer".into(),
                    original_name: "f".into(),
                    original_size: 0,
                    shard_size: 1,
                    created_unix: 0,
                    content_root_blake3: String::new(),
                    coding: None,
                    shards: vec![],
                },
            );
        }
        data.names.insert(
            "n0".into(),
            ExportName {
                entries: [(file.to_string(), path.to_string())].into(),
                parents: [(file.to_string(), BTreeSet::new())].into(),
                published: true,
            },
        );
        times.insert("n0".into(), 1);
        (data, times)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive_history::model::{Retention, VersionKind};

    const FILE: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    #[test]
    fn v7_trash_versions_times_and_restorable_bytes() {
        let (data, times) = fixture::export(
            FILE,
            "Docs/f.txt",
            &[
                ("r1", &[], Some("one"), 10),
                ("r2", &["r1"], Some("two"), 20),
                ("r3", &["r2"], None, 30),
            ],
            &["r2"],
        );
        let h = build(&data, &times, 100, BTreeSet::new()).unwrap();
        let k = |r: &str| key(FILE, r);
        assert_eq!(h.revs[&k("r2")].time, Some(20));
        assert_eq!(h.revs[&k("r2")].parents, [k("r1")]);
        let trash = crate::drive_history::trash::list(&h, &Retention::default(), 100).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(
            (trash[0].id.clone(), trash[0].path.as_str()),
            (k("r3"), "/Docs/f.txt")
        );
        let versions = crate::drive_history::versions::list(&h, "Docs/f.txt").unwrap();
        let kinds: Vec<_> = versions.iter().map(|v| (v.kind, v.restorable)).collect();
        assert_eq!(
            kinds,
            [
                (VersionKind::Deleted, false),
                (VersionKind::Modified, true),
                (VersionKind::Created, false)
            ]
        );
        assert_eq!(
            target(&h, &k("r2")).unwrap(),
            (FILE.to_string(), "r2".to_string())
        );
        // Purged marks of other drives are ignored.
        let h = build(&data, &times, 100, ["zzz".to_string(), k("r3")].into()).unwrap();
        assert_eq!(h.purged.len(), 1);
    }
}
