//! Read-only observation of the cloud for the storage manager (builds the
//! `classify::World`): the provider's listing; for every configured pool
//! that uses the provider or has metadata on it, every account's listing,
//! its recorded migrations and every drive generation's events (checkpoints
//! included); the drive metadata of pools not configured here (read from
//! this provider); the local inventory, every manifest replica on every
//! listed account and drive uploads no metadata names. Anything that cannot
//! be read is recorded as uncertain.
use super::classify::{
    generation_statuses, owned_archives, Generation, GenerationStatus, PoolScope, World,
};
use super::layout::{self, Listing, UnitKind};
use crate::migration::enumerate::RemoteListing;
use crate::migration::model::Action;
use crate::migration::retire::live_refs;
use crate::migration::retire::refs::References;
use crate::pool::browse_generations::Generation as Found;
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::rclone::RcloneContext;
use crate::storage::reader::StorageReader;
use crate::storage::traits::OperationContext;

/// Prefix of drive metadata scopes in a root listing.
const EVENTS: &str = ".rpool-sync/events-v6/";
/// Prefix of migration journal scopes in a root listing.
const MIGRATIONS: &str = ".rpool-sync/migrations-v1/";

/// Recursive listings (with times) of `roots`, in parallel; a remote that is
/// not in the rclone config is not called.
pub(crate) fn list_roots(
    rclone: &str,
    roots: &BTreeSet<String>,
) -> BTreeMap<String, Result<Listing, String>> {
    let configured: Option<BTreeSet<String>> =
        RcloneAdmin::inherited(rclone).discover().ok().map(|names| {
            names
                .iter()
                .map(|n| n.trim_end_matches(':').to_owned())
                .collect()
        });
    let context = RcloneContext::inherited(rclone);
    roots
        .par_iter()
        .map(|root| {
            let name = root.split_once(':').map_or(root.as_str(), |(n, _)| n);
            if configured
                .as_ref()
                .is_some_and(|names| !names.contains(name))
            {
                return (root.clone(), Err("not in this PC's rclone config".into()));
            }
            let listing = match context.list_recursive(&OperationContext::none(), root) {
                Ok(bytes) => layout::parse_lsjson(&bytes).map_err(|e| format!("{e:#}")),
                Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {
                    Ok(Listing::new())
                }
                Err(error) => Err(error.to_string()),
            };
            (root.clone(), listing)
        })
        .collect()
}

/// A listing without times, as the migration reference sources take it.
fn plain(listing: &Result<Listing, String>) -> RemoteListing {
    match listing {
        Ok(files) => {
            RemoteListing::Listed(files.iter().map(|(p, e)| (p.clone(), e.size)).collect())
        }
        Err(error) => RemoteListing::Failed(error.clone()),
    }
}

/// Generations of `scope` found in `listings`, newest first (like
/// `browse_generations::discover`, from listings already made).
fn generations<'a>(listings: impl IntoIterator<Item = &'a Listing>, scope: &str) -> Vec<Found> {
    let prefix = format!("{EVENTS}{scope}/");
    let mut out: Vec<Found> = Vec::new();
    for listing in listings {
        for (path, entry) in listing.range(prefix.clone()..) {
            let Some(rest) = path.strip_prefix(&prefix) else {
                break;
            };
            let epoch = rest
                .strip_prefix("epochs/")
                .and_then(|r| r.split_once('/'))
                .map(|(e, _)| e.to_owned());
            if epoch.as_deref().is_some_and(|e| !layout::is_hex64(e)) {
                continue;
            }
            let time = entry
                .modified
                .map_or(i128::MIN, |t| i128::from(t) * 1_000_000_000);
            match out.iter_mut().find(|g| g.epoch == epoch) {
                Some(g) => {
                    g.newest = g.newest.max(time);
                    g.records += 1;
                }
                None => out.push(Found {
                    epoch,
                    newest: time,
                    records: 1,
                }),
            }
        }
    }
    crate::pool::browse_generations::sort(&mut out);
    out
}

/// Migration ids with a journal folder of `scope` in `listings`.
fn listed_migrations<'a>(
    listings: impl IntoIterator<Item = &'a Listing>,
    scope: &str,
) -> BTreeSet<String> {
    let prefix = format!("{MIGRATIONS}{scope}/");
    let mut out = BTreeSet::new();
    for listing in listings {
        for path in listing.range(prefix.clone()..).map(|(p, _)| p) {
            let Some(rest) = path.strip_prefix(&prefix) else {
                break;
            };
            if let Some((id, _)) = rest.split_once('/') {
                out.insert(id.to_owned());
            }
        }
    }
    out
}

/// Every string of every event of one generation (`events` already read),
/// and the archive folders its content manifests name
/// (`Generation::{refs, owns}`).
fn event_refs(
    label: &str,
    events: Result<BTreeMap<String, crate::mount::DriveEvent>>,
) -> (References, BTreeSet<String>) {
    let mut refs = References::default();
    match events {
        Ok(events) => {
            for event in events.values() {
                match serde_json::to_value(event) {
                    Ok(value) => refs.add_json(label, &value),
                    Err(error) => refs.uncertain(format!("{label}: {error:#}")),
                }
            }
            let manifests = events.values().filter_map(|e| e.content.as_ref());
            let owns = owned_archives(manifests.map(|c| &c.manifest));
            (refs, owns)
        }
        Err(error) => {
            refs.uncertain(format!("{label} could not be read: {error:#}"));
            (refs, BTreeSet::new())
        }
    }
}

/// Migrations of `pool`: in-progress ones keep what they name (`live`),
/// finished ones name their originals and abandoned ones their copies
/// (`leftovers`). Returns the abandoned ones (their generations are left
/// over). Every listed journal must be readable.
fn migrations(
    rclone: &str,
    pool: &str,
    listed: &BTreeSet<String>,
    live: &mut References,
    leftovers: &mut References,
    uncertain: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut abandoned = BTreeSet::new();
    let ids = match crate::migration::journal::discover(rclone, pool) {
        Ok(ids) => ids,
        Err(error) => {
            uncertain.push(format!("migrations of pool {pool}: {error:#}"));
            return abandoned;
        }
    };
    for id in listed.iter().filter(|id| !ids.contains(id)) {
        uncertain.push(format!("migration {id} of pool {pool}: no readable plan"));
    }
    for id in ids {
        let label = format!("migration {} of pool {pool}", &id[..id.len().min(12)]);
        let loaded = crate::migration::journal::Journal::open(rclone, pool, &id).and_then(|j| {
            let plan = j.load_plan()?.ok_or_else(|| anyhow!("no readable plan"))?;
            Ok((plan, j.records()?))
        });
        let (plan, records) = match loaded {
            Ok(found) => found,
            Err(error) => {
                uncertain.push(format!("{label}: {error:#}"));
                continue;
            }
        };
        let status = crate::migration::status::summarize(&plan, &records);
        let movable = plan.entries.iter().filter(|e| {
            matches!(
                e.action,
                Action::Relocate | Action::Reencode | Action::ReapplyPlacement
            )
        });
        let copies = records.iter().filter_map(|r| r.new_archive_id.as_deref());
        if status.abandoned {
            abandoned.insert(id.clone());
            copies.for_each(|c| leftovers.add_text(&label, c));
        } else if status.complete {
            movable.for_each(|e| leftovers.add_text(&label, &e.archive_id));
        } else {
            movable.for_each(|e| live.add_text(&label, &e.archive_id));
            copies.for_each(|c| live.add_text(&label, c));
        }
    }
    abandoned
}

/// Remote name and path without surrounding slashes of a `remote:path` root.
fn split_root(root: &str) -> (&str, &str) {
    let (name, path) = root.split_once(':').unwrap_or((root, ""));
    (name, path.trim_matches('/'))
}

/// Whether `a` and `b` (`remote:path`) name the same folder, ignoring
/// leading and trailing slashes (on some backends `/x` and `x` differ; they
/// count as the same so that a pool spelled either way keeps its data).
pub(crate) fn same_root(a: &str, b: &str) -> bool {
    split_root(a) == split_root(b)
}

/// Whether `a` and `b` (`remote:path`) are different roots on the same
/// remote where one lies inside the other.
fn overlaps(a: &str, b: &str) -> bool {
    let ((na, pa), (nb, pb)) = (split_root(a), split_root(b));
    if na != nb || pa == pb {
        return false;
    }
    let inside =
        |outer: &str, inner: &str| outer.is_empty() || inner.starts_with(&format!("{outer}/"));
    inside(pa, pb) || inside(pb, pa)
}

/// Builds the world of provider root `root` at `now`.
pub(crate) fn world(rclone: &str, root: &str, now: u64) -> Result<World> {
    let store = crate::pool::load_pool_store()?;
    let context = RcloneContext::inherited(rclone);
    let listing = match context.list_recursive(&OperationContext::none(), root) {
        Ok(bytes) => layout::parse_lsjson(&bytes)?,
        Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {
            Listing::new()
        }
        Err(error) => return Err(anyhow!(error).context(format!("cannot list {root}"))),
    };
    let mut uncertain = Vec::new();
    let units = layout::parse(&listing);
    let scopes_here: BTreeSet<&str> = units
        .values()
        .filter_map(|u| match &u.kind {
            UnitKind::Generation { scope, .. } | UnitKind::Migration { scope, .. } => {
                Some(scope.as_str())
            }
            _ => None,
        })
        .collect();
    // Configured pools that use this provider or have metadata on it.
    let mut pools: Vec<(PoolScope, PoolDefinition, Vec<String>)> = Vec::new();
    for (name, policy) in &store.pools {
        let scope = crate::mount::pool_sync::scope(name)?;
        let roots = match crate::remote_root::apply_remote_roots(policy.remotes.clone()) {
            Ok(roots) => roots
                .into_iter()
                .map(|r| r.trim_end_matches('/').to_owned())
                .collect::<Vec<_>>(),
            Err(error) => {
                uncertain.push(format!("pool {name}: {error:#}"));
                continue;
            }
        };
        for other in roots.iter().filter(|r| overlaps(r, root)) {
            uncertain.push(format!(
                "pool {name} stores data at {other}, inside or around {root}"
            ));
        }
        let uses_root = roots.iter().any(|r| same_root(r, root));
        if uses_root || scopes_here.contains(scope.as_str()) {
            let scope = PoolScope {
                name: name.clone(),
                scope,
                uses_root,
            };
            pools.push((scope, policy.clone(), roots));
        }
    }
    let mut roots: BTreeSet<String> = pools.iter().flat_map(|(_, _, r)| r.clone()).collect();
    roots.retain(|r| !same_root(r, root));
    let mut listings = list_roots(rclone, &roots);
    listings.insert(root.to_owned(), Ok(listing.clone()));
    for (other, result) in &listings {
        if let Err(error) = result {
            uncertain.push(format!("{other}: listing failed: {error}"));
        }
    }
    let mut live = References::default();
    let mut leftovers = References::default();
    let mut generations_out = Vec::new();
    for (pool, policy, pool_roots) in &pools {
        let pool_listings: Vec<&Listing> = pool_roots
            .iter()
            .map(|r| if same_root(r, root) { root } else { r.as_str() })
            .filter_map(|r| listings.get(r).and_then(|l| l.as_ref().ok()))
            .collect();
        let found = generations(pool_listings.iter().copied(), &pool.scope);
        let listed = listed_migrations(pool_listings.iter().copied(), &pool.scope);
        let abandoned = migrations(
            rclone,
            &pool.name,
            &listed,
            &mut live,
            &mut leftovers,
            &mut uncertain,
        );
        let statuses = match crate::migration::drive_journal::known(rclone, &pool.name) {
            Ok(known) => generation_statuses(&found, &known, &abandoned, now),
            Err(error) => {
                uncertain.push(format!("drive migrations of pool {}: {error:#}", pool.name));
                // Unknown: every generation is treated as in use.
                found
                    .iter()
                    .map(|g| (g.epoch.clone(), GenerationStatus::Current))
                    .collect()
            }
        };
        // The current generation first: `read_v6` keeps the browse cache on it.
        let mut ordered = statuses;
        ordered.sort_by_key(|(_, s)| *s != GenerationStatus::Current);
        for (epoch, status) in ordered {
            let label = format!(
                "drive generation {} of pool {}",
                epoch.as_deref().map_or("first", |e| &e[..12]),
                pool.name
            );
            let events =
                crate::mount::metadata_pool::read_v6(rclone, &pool.name, policy, epoch.as_deref());
            let (refs, owns) = event_refs(&label, events);
            generations_out.push(Generation {
                pool: Some(pool.name.clone()),
                scope: pool.scope.clone(),
                epoch,
                status,
                refs,
                owns,
            });
        }
    }
    // Pools not configured here: their metadata on this provider only.
    let configured_scopes: BTreeSet<String> = store
        .pools
        .keys()
        .filter_map(|name| crate::mount::pool_sync::scope(name).ok())
        .collect();
    for unit in units.values() {
        let UnitKind::Generation { scope, epoch } = &unit.kind else {
            continue;
        };
        if configured_scopes.contains(scope) {
            continue;
        }
        let mut path = crate::utils::remote_join(root, &format!("{EVENTS}{scope}"));
        if let Some(epoch) = epoch {
            path = crate::utils::remote_join(&path, &format!("epochs/{epoch}"));
        }
        let label = format!(
            "drive generation {} of foreign pool {}",
            epoch.as_deref().map_or("first", |e| &e[..12]),
            &scope[..12]
        );
        let events = crate::mount::metadata_pool::read_v6_at(rclone, &[path]);
        let (refs, owns) = event_refs(&label, events);
        generations_out.push(Generation {
            pool: None,
            scope: scope.clone(),
            epoch: epoch.clone(),
            status: GenerationStatus::Foreign,
            refs,
            owns,
        });
    }
    let plain_listings: BTreeMap<String, RemoteListing> = listings
        .iter()
        .map(|(r, l)| (r.clone(), plain(l)))
        .collect();
    let mut manifests = BTreeSet::new();
    let mut folders = BTreeSet::new();
    for files in plain_listings.values().filter_map(RemoteListing::files) {
        manifests.extend(
            crate::migration::enumerate::manifest_ids(files)
                .into_iter()
                .map(|(id, _)| id),
        );
        folders.extend(
            files
                .keys()
                .filter_map(|p| p.split_once('/').map(|(top, _)| top))
                .filter(|top| !top.starts_with('.'))
                .map(str::to_owned),
        );
    }
    // Folders some drive generation names: their manifests are not "unnamed uploads".
    let named: BTreeSet<String> = folders
        .into_iter()
        .filter(|f| {
            generations_out
                .iter()
                .any(|g| !g.refs.referrers(f, std::iter::empty()).is_empty())
        })
        .collect();
    let reader = StorageReader::rclone(rclone);
    let policy = PoolDefinition::default();
    let sources = live_refs::Sources {
        rclone,
        pool: "",
        policy: &policy,
        migration_id: "",
        listings: &plain_listings,
        subjects: &named,
        workspaces: &[],
        reader: &reader,
    };
    live_refs::inventory(&sources, &mut live);
    match crate::inventory::load_inventory() {
        Ok(inventory) => {
            for entry in inventory.entries.values() {
                live.add_text("this PC's inventory", &entry.archive_id);
            }
        }
        Err(error) => live.uncertain(format!("local inventory: {error:#}")),
    }
    live_refs::cloud_manifests(&sources, &mut live);
    crate::drive_history::cleanup::live::unnamed_uploads(&sources, &mut live);
    Ok(World {
        root: root.to_owned(),
        listing,
        pools: pools.into_iter().map(|(p, _, _)| p).collect(),
        generations: generations_out,
        live,
        leftovers,
        manifests,
        uncertain,
        now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::storage::layout::Entry;

    #[test]
    fn generations_and_migrations_come_from_listings() {
        let scope = "a".repeat(64);
        let epoch = "b".repeat(64);
        let at = |t: u64| Entry {
            size: 1,
            modified: Some(t),
        };
        let listing: Listing = [
            (format!("{EVENTS}{scope}/events/x.json"), at(10)),
            (
                format!("{EVENTS}{scope}/epochs/{epoch}/events/y.json"),
                at(20),
            ),
            (
                format!("{EVENTS}{scope}/epochs/{epoch}/events/z.json"),
                at(30),
            ),
            (format!("{MIGRATIONS}{scope}/m1/plan.json"), at(5)),
            ("virtual-a/manifest.json".into(), at(1)),
        ]
        .into();
        let found = generations([&listing], &scope);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].epoch.as_deref(), Some(epoch.as_str()));
        assert_eq!(found[0].records, 2);
        assert!(found[0].newest > found[1].newest);
        assert_eq!(
            listed_migrations([&listing], &scope),
            ["m1".to_string()].into()
        );
        assert!(overlaps("a:x", "a:x/y") && overlaps("a:", "a:x"));
        assert!(same_root("a:/x/", "a:x") && !same_root("a:x", "b:x"));
        assert!(!overlaps("a:x", "a:x") && !overlaps("a:x", "b:x/y") && !overlaps("a:x", "a:xy"));
    }
}
