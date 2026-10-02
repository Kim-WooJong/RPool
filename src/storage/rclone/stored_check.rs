//! Deferred upload verification: the provider hashes of a whole archive,
//! one recursive listing per account instead of one call per shard.
use super::*;
use crate::storage::stored_hash::Expected;

/// How deep below the listed folder an object may sit. An archive is
/// `<archive>/{data,parity}/<shard>`, so listing the archive folder needs 2;
/// anything shallower in common is listed object by object instead, which
/// keeps a listing from ever walking a whole account.
const MAX_DEPTH: usize = 2;

impl RcloneContext {
    /// For every item, whether the provider reports exactly its size and
    /// hash. Accounts are checked concurrently; any doubt is `false`.
    pub(crate) fn check_stored_hashes(
        &self,
        ctx: &OperationContext,
        items: &[Expected],
    ) -> Vec<bool> {
        let mut groups: std::collections::BTreeMap<(String, String), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, item) in items.iter().enumerate() {
            if let Ok(name) = remote_name(&item.address) {
                groups
                    .entry((name.to_owned(), item.kind.clone()))
                    .or_default()
                    .push(i);
            }
        }
        let mut ok = vec![false; items.len()];
        let answers: Vec<Vec<(usize, bool)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = groups
                .values()
                .map(|members| scope.spawn(move || self.check_group(ctx, items, members)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for (i, good) in answers.into_iter().flatten() {
            ok[i] = good;
        }
        ok
    }

    /// Checks one (account, hash type) group: one recursive `lsjson --hash` of their
    /// deepest common folder, or per-object stat-with-hash when there is no shallow common
    /// folder (see [`MAX_DEPTH`]), fewer than two members, or the listing fails.
    /// Returns `(item index, size and hash match)`.
    fn check_group(
        &self,
        ctx: &OperationContext,
        items: &[Expected],
        members: &[usize],
    ) -> Vec<(usize, bool)> {
        let single = |i: usize| {
            let item = &items[i];
            let good = matches!(
                self.object_hash(ctx, &item.address, std::slice::from_ref(&item.kind)),
                Ok(Some((size, reported)))
                    if size == item.size
                        && reported.rsplit(':').next().is_some_and(|v| v.eq_ignore_ascii_case(&item.value))
            );
            (i, good)
        };
        let Some(folder) = common_folder(members.iter().map(|&i| items[i].address.as_str())) else {
            return members.iter().map(|&i| single(i)).collect();
        };
        if members.len() < 2
            || members
                .iter()
                .any(|&i| items[i].address[folder.len()..].matches('/').count() > MAX_DEPTH)
        {
            return members.iter().map(|&i| single(i)).collect();
        }
        let kind = &items[members[0]].kind;
        let listing = self.capture(
            ctx,
            &[
                "lsjson",
                "-R",
                "--files-only",
                "--hash",
                "--hash-type",
                kind,
                "--",
                &folder,
            ],
        );
        let Ok(bytes) = listing else {
            return members.iter().map(|&i| single(i)).collect();
        };
        let Ok(Value::Array(entries)) = serde_json::from_slice::<Value>(&bytes) else {
            return members.iter().map(|&i| single(i)).collect();
        };
        let mut found: std::collections::BTreeMap<String, (u64, String)> =
            std::collections::BTreeMap::new();
        for entry in entries {
            let path = entry.get("Path").and_then(Value::as_str);
            let size = entry.get("Size").and_then(Value::as_u64);
            let hash = entry
                .get("Hashes")
                .and_then(|h| h.get(kind.as_str()))
                .and_then(Value::as_str);
            if let (Some(path), Some(size), Some(hash)) = (path, size, hash) {
                found.insert(path.to_owned(), (size, hash.to_ascii_lowercase()));
            }
        }
        members
            .iter()
            .map(|&i| {
                let item = &items[i];
                let relative = item.address[folder.len()..].trim_start_matches('/');
                let good = found.get(relative).is_some_and(|(size, hash)| {
                    *size == item.size && hash.eq_ignore_ascii_case(&item.value)
                });
                (i, good)
            })
            .collect()
    }
}

/// Deepest folder (`remote:path`, no trailing slash) containing every
/// address, below the remote root; None when they share only the root.
fn common_folder<'a>(mut addresses: impl Iterator<Item = &'a str>) -> Option<String> {
    let first = addresses.next()?;
    let folder = |a: &'a str| a.rsplit_once('/').map(|(dir, _)| dir.to_owned());
    let mut common = folder(first)?;
    for address in addresses {
        while !(address.starts_with(&common) && address[common.len()..].starts_with('/')) {
            common = common.rsplit_once('/').map(|(dir, _)| dir.to_owned())?;
        }
    }
    let (_, path) = common.split_once(':')?;
    (!path.trim_matches('/').is_empty()).then_some(common)
}

#[cfg(test)]
mod tests {
    use super::common_folder;

    #[test]
    fn common_folder_stays_below_the_root() {
        let a = ["c:/r/arch/data/1", "c:/r/arch/parity/2"];
        assert_eq!(common_folder(a.into_iter()).as_deref(), Some("c:/r/arch"));
        assert_eq!(
            common_folder(["c:arch/data/1"].into_iter()).as_deref(),
            Some("c:arch/data")
        );
        assert_eq!(common_folder(["c:a/1", "c:b/2"].into_iter()), None);
        assert_eq!(common_folder(["c:1"].into_iter()), None);
        assert_eq!(
            common_folder(["c:/r/arch/data/12", "c:/r/arch/data/1"].into_iter()).as_deref(),
            Some("c:/r/arch/data")
        );
        // A sibling whose name extends the folder is not inside it.
        assert_eq!(
            common_folder(["c:/r/ab/x", "c:/r/abc/y"].into_iter()).as_deref(),
            Some("c:/r")
        );
    }
}
