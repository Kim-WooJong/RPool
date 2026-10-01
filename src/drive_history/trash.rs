//! Trash: files whose every head revision is a deletion, newest first.
//!
//! - The entry id is the deletion revision; its bytes are the nearest earlier
//!   revision with content (`History::last_content`).
//! - Hidden: purged entries, expired entries (retention), and deletions whose
//!   bytes are still visible in the drive (a move, a restored copy, or an
//!   identical copy elsewhere). Among deleted files with identical bytes only
//!   the newest deletion is listed (a moved-then-deleted file appears once).
//! - A folder entry (`dir:/path`) aggregates a folder that no longer exists
//!   and held at least two trashed files; those files are listed as well.
use super::graph::{collides, display_path, in_scope, History};
use super::model::{Retention, TrashEntry};
use super::restore::{unique_name, Action};
use crate::prelude::*;

pub(crate) const DIR_PREFIX: &str = "dir:";

/// One trashed file: entry plus the revision whose bytes come back.
#[derive(Debug, Clone)]
pub(crate) struct Trashed {
    pub entry: TrashEntry,
    /// Namespace path (no leading `/`).
    pub path: String,
    pub bytes_rev: String,
}

/// Every listed trashed file (no folder entries).
pub(crate) fn files(history: &History, retention: &Retention, now: u64) -> Result<Vec<Trashed>> {
    let current = history.view_at(None)?;
    let visible: BTreeSet<&str> = current
        .values()
        .filter_map(|id| history.revs[id].content.as_ref().map(|c| c.hash.as_str()))
        .collect();
    let mut by_hash: BTreeMap<String, Trashed> = BTreeMap::new();
    for (lineage, mut heads) in history.deleted_lineages() {
        if heads.iter().any(|id| history.purged.contains(id)) {
            continue;
        }
        history.newest_first(&mut heads);
        let deletion = heads[0].clone();
        let Some(bytes_rev) = history.last_content(&deletion) else {
            continue;
        };
        let content = history.revs[&bytes_rev].content.clone().unwrap();
        if visible.contains(content.hash.as_str()) {
            continue;
        }
        let rev = &history.revs[&deletion];
        let expires = super::retention::trash_expiry(rev.time, retention);
        if expires.is_some_and(|t| t <= now) {
            continue;
        }
        let path = lineage.clone();
        let entry = TrashEntry {
            id: deletion.clone(),
            path: display_path(&path),
            is_dir: false,
            size: content.size,
            deleted_unix: rev.time,
            deleted_by: Some(rev.author.clone()),
            expires_unix: expires,
            path_taken: current.keys().any(|p| collides(p, &path)),
        };
        let item = Trashed {
            entry,
            path,
            bytes_rev,
        };
        match by_hash.get(&content.hash) {
            Some(old) if history.effective(&old.entry.id) >= history.effective(&deletion) => {}
            _ => {
                by_hash.insert(content.hash.clone(), item);
            }
        }
    }
    let mut result: Vec<_> = by_hash.into_values().collect();
    result.sort_by(|a, b| {
        (b.entry.deleted_unix, &a.entry.path).cmp(&(a.entry.deleted_unix, &b.entry.path))
    });
    Ok(result)
}

/// Trash listing: folder entries first, then files (newest deletion first).
pub(crate) fn list(history: &History, retention: &Retention, now: u64) -> Result<Vec<TrashEntry>> {
    let files = files(history, retention, now)?;
    let current = history.view_at(None)?;
    let mut entries = folders(&files, &current);
    entries.extend(files.into_iter().map(|t| t.entry));
    Ok(entries)
}

fn folders(files: &[Trashed], current: &BTreeMap<String, String>) -> Vec<TrashEntry> {
    let mut dirs: BTreeMap<String, Vec<&Trashed>> = BTreeMap::new();
    for item in files {
        let mut prefix = String::new();
        let parts: Vec<_> = item.path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            dirs.entry(prefix.clone()).or_default().push(item);
        }
    }
    // A folder qualifies when it is gone (no current file inside) and held
    // at least two trashed files; only the top-most qualifying folder is shown.
    let qualifies = |dir: &str, members: &[&Trashed]| {
        members.len() >= 2
            && !current
                .keys()
                .any(|p| in_scope(p, dir) && p.len() > dir.len())
    };
    let mut result = Vec::new();
    for (dir, members) in &dirs {
        if !qualifies(dir, members) {
            continue;
        }
        let parent_qualifies = dir
            .rsplit_once('/')
            .is_some_and(|(parent, _)| dirs.get(parent).is_some_and(|m| qualifies(parent, m)));
        if parent_qualifies {
            continue;
        }
        let authors: BTreeSet<_> = members
            .iter()
            .filter_map(|m| m.entry.deleted_by.clone())
            .collect();
        let expires = members
            .iter()
            .map(|m| m.entry.expires_unix)
            .collect::<Option<Vec<_>>>()
            .and_then(|all| all.into_iter().max());
        result.push(TrashEntry {
            id: format!("{DIR_PREFIX}{}", display_path(dir)),
            path: display_path(dir),
            is_dir: true,
            size: members.iter().map(|m| m.entry.size).sum(),
            deleted_unix: members.iter().filter_map(|m| m.entry.deleted_unix).max(),
            deleted_by: (authors.len() == 1).then(|| authors.into_iter().next().unwrap()),
            expires_unix: expires,
            path_taken: current.keys().any(|p| p.eq_ignore_ascii_case(dir)),
        });
    }
    result
}

/// Trashed files selected by entry ids (file ids or `dir:/path`).
pub(crate) fn select<'a>(files: &'a [Trashed], ids: &[String]) -> Result<Vec<&'a Trashed>> {
    let mut selected: Vec<&Trashed> = Vec::new();
    for id in ids {
        let found: Vec<&Trashed> = match id.strip_prefix(DIR_PREFIX) {
            Some(dir) => {
                let dir = super::graph::namespace_path(dir)?;
                files.iter().filter(|t| in_scope(&t.path, &dir)).collect()
            }
            None => files.iter().filter(|t| &t.entry.id == id).collect(),
        };
        if found.is_empty() {
            bail!("not in the trash (restored, purged, expired or unknown id): {id}");
        }
        for item in found {
            if !selected.iter().any(|s| s.entry.id == item.entry.id) {
                selected.push(item);
            }
        }
    }
    Ok(selected)
}

/// Restore actions: back to the original path, or `name (restored).ext` when
/// it is taken; a folder restores every trashed file inside it. `to`
/// replaces the original path of a single entry (file or folder).
pub(crate) fn restore_actions(
    history: &History,
    files: &[Trashed],
    ids: &[String],
    to: Option<&str>,
    into: Option<&str>,
) -> Result<Vec<Action>> {
    if to.is_some() && ids.len() != 1 {
        bail!("--to needs exactly one --id");
    }
    if to.is_some() && into.is_some() {
        bail!("use --to or --into, not both");
    }
    let into = into.map(super::graph::namespace_path).transpose()?;
    // `into`: the entry's own name under that folder.
    let inside = |path: &str| -> String {
        let name = path.rsplit('/').next().unwrap_or(path);
        match into.as_deref() {
            Some("") | None => name.to_owned(),
            Some(folder) => format!("{folder}/{name}"),
        }
    };
    let selected = select(files, ids)?;
    let mut taken: Vec<String> = history.view_at(None)?.into_keys().collect();
    let mut actions = Vec::new();
    for id in ids {
        let members: Vec<&&Trashed> = match id.strip_prefix(DIR_PREFIX) {
            Some(dir) => {
                let dir = super::graph::namespace_path(dir)?;
                let base = match (to, &into) {
                    (Some(to), _) => super::graph::namespace_path(to)?,
                    (None, Some(_)) => inside(&dir),
                    (None, None) => dir.clone(),
                };
                if base.is_empty() {
                    bail!("cannot restore a folder onto the drive root");
                }
                // The folder name itself is taken by a file: restore beside it.
                let base = if taken.iter().any(|p| p.eq_ignore_ascii_case(&base)) {
                    unique_name(&base, &taken)
                } else {
                    base
                };
                for item in selected.iter().filter(|t| in_scope(&t.path, &dir)) {
                    let target = format!("{base}{}", &item.path[dir.len()..]);
                    push_restore(&mut actions, &mut taken, item, target);
                }
                continue;
            }
            None => selected.iter().filter(|t| &t.entry.id == id).collect(),
        };
        for item in members {
            if actions.iter().any(|a: &Action| a.rev() == item.bytes_rev) {
                continue;
            }
            let target = match (to, &into) {
                (Some(to), _) => super::graph::namespace_path(to)?,
                (None, Some(_)) => inside(&item.path),
                (None, None) => item.path.clone(),
            };
            push_restore(&mut actions, &mut taken, item, target);
        }
    }
    Ok(actions)
}

fn push_restore(
    actions: &mut Vec<Action>,
    taken: &mut Vec<String>,
    item: &Trashed,
    target: String,
) {
    if actions.iter().any(|a| a.rev() == item.bytes_rev) {
        return;
    }
    let target = if taken.iter().any(|p| collides(p, &target)) {
        unique_name(&target, taken)
    } else {
        target
    };
    taken.push(target.clone());
    actions.push(Action::Put {
        path: target,
        rev: item.bytes_rev.clone(),
        from: Some(item.path.clone()),
    });
}

/// Ids a purge marks: the selected entries, or every expired-but-unpurged
/// trash entry (`expired`), or everything in the trash (`all`).
pub(crate) fn purge_selection(
    history: &History,
    retention: &Retention,
    now: u64,
    ids: &[String],
    expired: bool,
    all: bool,
) -> Result<BTreeSet<String>> {
    if expired {
        let mut selected = BTreeSet::new();
        for (_, heads) in history.deleted_lineages() {
            if heads.iter().any(|id| history.purged.contains(id)) {
                continue;
            }
            for head in heads {
                let expiry = super::retention::trash_expiry(history.revs[&head].time, retention);
                if expiry.is_some_and(|t| t <= now) && history.last_content(&head).is_some() {
                    selected.insert(head);
                }
            }
        }
        return Ok(selected);
    }
    let files = files(history, retention, now)?;
    if all {
        return Ok(files.into_iter().map(|t| t.entry.id).collect());
    }
    if ids.is_empty() {
        bail!("choose --id ID... or --expired");
    }
    Ok(select(&files, ids)?
        .into_iter()
        .map(|t| t.entry.id.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev, Fx};
    use super::*;

    fn sample() -> Fx {
        history(vec![
            ("a1", rev("a.txt", &[], Some("alpha"), "pc", Some(10))),
            ("a2", rev("a.txt", &["a1"], None, "PC-B", Some(20))),
            ("b1", rev("Docs/b.txt", &[], Some("beta"), "pc", Some(10))),
            ("b2", rev("Docs/b.txt", &["b1"], None, "pc", Some(30))),
            (
                "c1",
                rev("Docs/sub/c.txt", &[], Some("gamma"), "pc", Some(10)),
            ),
            ("c2", rev("Docs/sub/c.txt", &["c1"], None, "pc", Some(31))),
            // Live file whose path (other case) the deleted a.txt used.
            ("n1", rev("A.txt", &[], Some("new"), "pc", Some(40))),
            // A moved file: deleted at its old path, same bytes live.
            ("m1", rev("old.txt", &[], Some("moved"), "pc", Some(10))),
            ("m2", rev("old.txt", &["m1"], None, "pc", Some(11))),
            (
                "m3",
                rev("new-place.txt", &[], Some("moved"), "pc", Some(11)),
            ),
        ])
    }

    #[test]
    fn lists_trash_with_folders_taken_paths_and_hides_moves() {
        let h = sample();
        let entries = list(&h, &Retention::default(), 100).unwrap();
        let ids: Vec<_> = entries.iter().map(|e| h.name(&e.id)).collect();
        assert_eq!(ids, ["dir:/Docs", "c2", "b2", "a2"]);
        let a = entries.iter().find(|e| e.id == h.id("a2")).unwrap();
        assert!(a.path_taken);
        assert_eq!(a.deleted_by.as_deref(), Some("PC-B"));
        assert_eq!(a.size, 5);
        assert_eq!(a.expires_unix, Some(20 + 30 * 86_400));
        let dir = &entries[0];
        assert!(dir.is_dir && dir.size == 9 && !dir.path_taken);
        // Expired entries leave the trash.
        let later = list(&h, &Retention::default(), 21 + 30 * 86_400).unwrap();
        assert!(!later.iter().any(|e| e.id == h.id("a2")));
    }

    #[test]
    fn restore_to_original_taken_path_and_folder() {
        let h = sample();
        let files = files(&h, &Retention::default(), 100).unwrap();
        let actions = restore_actions(&h, &files, &[h.id("a2")], None, None).unwrap();
        assert_eq!(
            actions,
            [Action::Put {
                path: "a (restored).txt".into(),
                rev: h.id("a1"),
                from: Some("a.txt".into())
            }]
        );
        let actions = restore_actions(&h, &files, &["dir:/Docs".into()], None, None).unwrap();
        let paths: BTreeSet<_> = actions.iter().map(|a| a.path().to_owned()).collect();
        assert_eq!(
            paths,
            ["Docs/b.txt".to_string(), "Docs/sub/c.txt".into()].into()
        );
        let actions =
            restore_actions(&h, &files, &["dir:/Docs".into()], Some("/Restored"), None).unwrap();
        assert!(actions.iter().any(|a| a.path() == "Restored/sub/c.txt"));
        assert!(restore_actions(&h, &files, &["zzz".into()], None, None).is_err());
        assert!(restore_actions(&h, &files, &[h.id("a2"), h.id("b2")], Some("/x"), None).is_err());
        // --into keeps each entry's name under the chosen folder.
        let into =
            restore_actions(&h, &files, &["dir:/Docs".into()], None, Some("/Saved")).unwrap();
        assert!(
            into.iter().all(|a| a.path().starts_with("Saved/Docs")),
            "{into:?}"
        );
    }

    #[test]
    fn purge_selection_by_id_expiry_and_all() {
        let mut h = sample();
        let (a2, b2, c2) = (h.id("a2"), h.id("b2"), h.id("c2"));
        let r = Retention::default();
        assert_eq!(
            purge_selection(&h, &r, 100, &["dir:/Docs".into()], false, false).unwrap(),
            [b2.clone(), c2.clone()].into()
        );
        let late = 31 + 30 * 86_400;
        let expired = purge_selection(&h, &r, late, &[], true, false).unwrap();
        assert!(expired.contains(&a2) && expired.contains(&b2) && expired.contains(&c2));
        assert!(purge_selection(&h, &r, 100, &[], false, false).is_err());
        h.purged.insert(a2.clone());
        let all = purge_selection(&h, &r, 100, &[], false, true).unwrap();
        assert!(!all.contains(&a2) && all.contains(&b2));
        assert!(!list(&h, &r, 100).unwrap().iter().any(|e| e.id == a2));
    }
}
