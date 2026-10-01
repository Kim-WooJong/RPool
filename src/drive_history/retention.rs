//! Per-pool retention (`rpool drive retention`) and which revisions it keeps.
//!
//! Saved in the pool store (`pools.json`, `retention` map by pool name) so it
//! travels with portable export/import; a missing entry means the defaults.
//! Retention never deletes anything by itself: it decides which trash entries
//! are still listed and which old revisions must keep their data (the drive
//! cleanup, `cleanup`, deletes only what it does not keep).
use super::graph::History;
use super::model::{CleanupSettings, Retention};
use crate::prelude::*;

pub(crate) const DAY: u64 = 86_400;
const MAX_DAYS: u32 = 36_500;
const MAX_VERSIONS: u32 = 10_000;

pub(crate) fn validate(retention: &Retention) -> Result<()> {
    if retention.trash_days > MAX_DAYS || retention.version_days > MAX_DAYS {
        bail!("retention days must be between 0 and {MAX_DAYS} (0 = unlimited)");
    }
    if retention.keep_versions > MAX_VERSIONS {
        bail!("keep versions must be between 0 and {MAX_VERSIONS} (0 = unlimited)");
    }
    Ok(())
}

/// Retention of `pool` (defaults when never set).
pub(crate) fn load(pool: &str) -> Result<Retention> {
    let store = crate::pool::load_pool_store()?;
    if !store.pools.contains_key(pool) {
        bail!("pool not found: {pool}");
    }
    Ok(store.retention.get(pool).copied().unwrap_or_default())
}

/// Changes the given fields of `pool`'s retention and saves the pool store.
pub(crate) fn update(
    pool: &str,
    trash_days: Option<u32>,
    keep_versions: Option<u32>,
    version_days: Option<u32>,
) -> Result<Retention> {
    let mut store = crate::pool::load_pool_store()?;
    if !store.pools.contains_key(pool) {
        bail!("pool not found: {pool}");
    }
    let mut value = store.retention.get(pool).copied().unwrap_or_default();
    value.trash_days = trash_days.unwrap_or(value.trash_days);
    value.keep_versions = keep_versions.unwrap_or(value.keep_versions);
    value.version_days = version_days.unwrap_or(value.version_days);
    validate(&value)?;
    store.retention.insert(pool.to_owned(), value);
    crate::pool::save_pool_store(&store)?;
    Ok(value)
}

/// Longest cleanup grace period (days).
pub(crate) const MAX_GRACE_DAYS: u32 = 3_650;

/// Automatic cleanup settings of `pool` (defaults when never set).
pub(crate) fn load_cleanup(pool: &str) -> Result<CleanupSettings> {
    let store = crate::pool::load_pool_store()?;
    if !store.pools.contains_key(pool) {
        bail!("pool not found: {pool}");
    }
    Ok(store.drive_cleanup.get(pool).copied().unwrap_or_default())
}

/// Changes the given cleanup settings of `pool` and saves the pool store.
/// The grace period is at least one day: it is what lets every PC publish
/// (or restore) before marked data goes.
pub(crate) fn update_cleanup(
    pool: &str,
    auto: Option<bool>,
    grace_days: Option<u32>,
) -> Result<CleanupSettings> {
    let mut store = crate::pool::load_pool_store()?;
    if !store.pools.contains_key(pool) {
        bail!("pool not found: {pool}");
    }
    let mut value = store.drive_cleanup.get(pool).copied().unwrap_or_default();
    value.auto = auto.unwrap_or(value.auto);
    value.grace_days = grace_days.unwrap_or(value.grace_days);
    if !(1..=MAX_GRACE_DAYS).contains(&value.grace_days) {
        bail!("cleanup grace must be between 1 and {MAX_GRACE_DAYS} days");
    }
    store.drive_cleanup.insert(pool.to_owned(), value);
    crate::pool::save_pool_store(&store)?;
    Ok(value)
}

/// When a deletion at `deleted` leaves the trash (`None`: kept until purged,
/// either unlimited trash days or an unknown deletion time).
pub(crate) fn trash_expiry(deleted: Option<u64>, retention: &Retention) -> Option<u64> {
    if retention.trash_days == 0 {
        return None;
    }
    deleted.map(|time| time.saturating_add(u64::from(retention.trash_days) * DAY))
}

/// Revisions whose data retention keeps: the deleted bytes of every trash
/// entry that has not expired or been purged, and per live file the newest
/// `keep_versions` previous versions not older than `version_days` (measured
/// from when the next revision replaced them). `clock` is the time used for
/// expiry (a missing time protects: it is never treated as expired).
pub(crate) fn protected(
    history: &History,
    retention: &Retention,
    now: u64,
    clock: &dyn Fn(&str) -> Option<u64>,
) -> BTreeSet<String> {
    let mut keep = BTreeSet::new();
    for (lineage, heads) in history.heads_at(None) {
        let deleted = heads.iter().all(|id| history.revs[id].content.is_none());
        if deleted {
            if heads.iter().any(|id| history.purged.contains(id)) {
                continue;
            }
            for head in &heads {
                let expired = trash_expiry(clock(head), retention).is_some_and(|t| t <= now);
                if !expired {
                    // The deleted bytes, and its versions as for a live file
                    // (a restored file keeps its history).
                    keep.extend(history.last_content(head));
                    versions_kept(history, head, retention, now, clock, &mut keep);
                }
            }
            continue;
        }
        let _ = lineage;
        for head in &heads {
            versions_kept(history, head, retention, now, clock, &mut keep);
        }
    }
    keep
}

/// Walks the content ancestors of `head` newest first.
fn versions_kept(
    history: &History,
    head: &str,
    retention: &Retention,
    now: u64,
    clock: &dyn Fn(&str) -> Option<u64>,
    keep: &mut BTreeSet<String>,
) {
    let mut count = 0u32;
    // (revision, the revision that replaced it)
    let mut level: Vec<(String, String)> = history.revs[head]
        .parents
        .iter()
        .map(|p| (p.clone(), head.to_owned()))
        .collect();
    let mut seen = BTreeSet::new();
    while !level.is_empty() {
        let mut next = Vec::new();
        for (id, successor) in level {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(rev) = history.revs.get(&id) else {
                continue;
            };
            let mut replaced_by = successor;
            if rev.content.is_some() {
                count += 1;
                if retention.keep_versions != 0 && count > retention.keep_versions {
                    return;
                }
                let replaced = clock(&replaced_by);
                let expired = retention.version_days != 0
                    && replaced.is_some_and(|t| {
                        t.saturating_add(u64::from(retention.version_days) * DAY) <= now
                    });
                if expired {
                    // Older revisions were replaced even earlier.
                    continue;
                }
                keep.insert(id.clone());
                replaced_by = id.clone();
            }
            next.extend(rev.parents.iter().map(|p| (p.clone(), replaced_by.clone())));
        }
        level = next;
    }
}

/// Bytes of shard objects referenced only by revisions outside `keep` and
/// outside the current view (what a physical cleanup could reclaim).
pub(crate) fn eligible_bytes(history: &History, keep: &BTreeSet<String>) -> Result<u64> {
    let current: BTreeSet<String> = history.view_at(None)?.into_values().collect();
    let mut kept_objects = BTreeSet::new();
    let mut candidates = BTreeMap::new();
    for (id, payload) in &history.payloads {
        let manifest = &payload.manifest;
        let kept = keep.contains(id) || current.contains(id);
        for shard in &manifest.shards {
            if kept {
                kept_objects.insert(shard.object.clone());
            } else {
                candidates.insert(shard.object.clone(), shard.size);
            }
        }
    }
    Ok(candidates
        .iter()
        .filter(|(object, _)| !kept_objects.contains(*object))
        .map(|(_, size)| *size)
        .sum())
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev};
    use super::*;

    fn retention(trash_days: u32, keep_versions: u32, version_days: u32) -> Retention {
        Retention {
            trash_days,
            keep_versions,
            version_days,
        }
    }

    #[test]
    fn trash_expiry_follows_days_and_unknown_times_never_expire() {
        let r = retention(30, 20, 90);
        assert_eq!(trash_expiry(Some(10), &r), Some(10 + 30 * DAY));
        assert_eq!(trash_expiry(None, &r), None);
        assert_eq!(trash_expiry(Some(10), &retention(0, 20, 90)), None);
        assert!(validate(&retention(36_501, 0, 0)).is_err());
        assert!(validate(&retention(0, 10_001, 0)).is_err());
        assert!(validate(&Retention::default()).is_ok());
    }

    #[test]
    fn protection_keeps_unexpired_trash_and_recent_versions_only() {
        let day = DAY;
        let h = history(vec![
            // Deleted file: deleted on day 10.
            ("d1", rev("d.txt", &[], Some("gone"), "pc", Some(day))),
            ("d2", rev("d.txt", &["d1"], None, "pc", Some(10 * day))),
            // Live file with versions v1..v4 (v4 current).
            ("v1", rev("v.txt", &[], Some("1"), "pc", Some(day))),
            ("v2", rev("v.txt", &["v1"], Some("2"), "pc", Some(20 * day))),
            ("v3", rev("v.txt", &["v2"], Some("3"), "pc", Some(50 * day))),
            ("v4", rev("v.txt", &["v3"], Some("4"), "pc", Some(55 * day))),
        ]);
        let clock = |id: &str| h.revs.get(id).and_then(|r| r.time);
        let now = 60 * day;
        let has = |keep: &BTreeSet<String>, name: &str| keep.contains(&h.id(name));
        // Trash 30 days: deleted day 10 expired at day 40.
        let keep = protected(&h, &retention(30, 2, 0), now, &clock);
        assert!(!has(&keep, "d1"));
        assert!(has(&keep, "v3") && has(&keep, "v2") && !has(&keep, "v1"));
        // Trash 90 days keeps it; versions 20 days back: v3 (replaced day 55)
        // and v2 (replaced day 50) stay, v1 (replaced day 20) expired.
        let keep = protected(&h, &retention(90, 0, 20), now, &clock);
        assert!(has(&keep, "d1"));
        assert!(has(&keep, "v3") && has(&keep, "v2") && !has(&keep, "v1"));
        // Unknown times are never expired.
        let keep = protected(&h, &retention(1, 0, 1), now, &|_| None);
        assert!(has(&keep, "d1") && has(&keep, "v1"));
    }

    #[test]
    fn purged_trash_is_not_protected() {
        let mut h = history(vec![
            ("d1", rev("d.txt", &[], Some("gone"), "pc", Some(1))),
            ("d2", rev("d.txt", &["d1"], None, "pc", Some(2))),
        ]);
        let d2 = h.id("d2");
        h.purged.insert(d2);
        assert!(protected(&h, &Retention::default(), 3, &|_| Some(2)).is_empty());
    }
}
