//! Classification of one archive against the target policy (before probing).
use crate::prelude::*;

/// Why an archive must be re-encoded, or None when its coding already matches.
/// Empty archives cannot carry Reed-Solomon coding and are never re-encoded.
pub(crate) fn reencode_reason(manifest: &Manifest, target: &PoolDefinition) -> Option<String> {
    if manifest.original_size == 0 {
        return None;
    }
    let have = manifest
        .coding
        .as_ref()
        .map(|c| (c.data_shards, c.parity_shards));
    let want = (target.parity_shards > 0).then_some((target.data_shards, target.parity_shards));
    let mut reasons = Vec::new();
    if have != want {
        let show = |c: Option<(usize, usize)>| {
            c.map_or("no parity".to_owned(), |(k, m)| format!("RS {k}+{m}"))
        };
        reasons.push(format!("coding {} -> {}", show(have), show(want)));
    }
    match target.shard_bytes() {
        Ok(bytes)
            if !crate::models::shard_size::shard_size_matches(
                manifest.shard_size,
                manifest.original_size,
                bytes.get(),
                target.data_shards,
                target.parity_shards,
            ) =>
        {
            reasons.push(format!(
                "shard size {} -> {} bytes",
                manifest.shard_size,
                bytes.get()
            ));
        }
        _ => {}
    }
    (!reasons.is_empty()).then(|| reasons.join(", "))
}

/// Shards that must move to other target remotes: every shard on a remote no
/// longer in the pool, plus (Resilient placement only) shards exceeding M per
/// declared outage group within a coding group. Returns the shard indexes and
/// human-readable reasons.
pub(crate) fn shards_to_move(
    manifest: &Manifest,
    target: &PoolDefinition,
    target_remotes: &BTreeSet<String>,
    failure_domain: &dyn Fn(&str) -> Option<String>,
) -> (BTreeSet<u32>, Vec<String>) {
    let mut moving: BTreeSet<u32> = BTreeSet::new();
    let mut reasons = Vec::new();
    let removed: BTreeSet<&str> = manifest
        .shards
        .iter()
        .filter(|s| !target_remotes.contains(&s.remote))
        .map(|s| {
            moving.insert(s.index);
            s.remote.as_str()
        })
        .collect();
    if !removed.is_empty() {
        reasons.push(format!(
            "{} shard(s) on remotes no longer in the pool: {}",
            moving.len(),
            removed.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let Some(coding) = &manifest.coding else {
        return (moving, reasons);
    };
    if target.placement != Placement::Resilient || coding.parity_shards == 0 {
        return (moving, reasons);
    }
    let mut domains: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for shard in &manifest.shards {
        domains
            .entry(shard.remote.as_str())
            .or_insert_with(|| failure_domain(&shard.remote));
    }
    if manifest
        .shards
        .iter()
        .any(|s| !moving.contains(&s.index) && domains[s.remote.as_str()].is_none())
    {
        reasons
            .push("outage groups are not declared for every remote; placement not checked".into());
        return (moving, reasons);
    }
    let mut excess = 0usize;
    let mut per_group: BTreeMap<(u32, &str), Vec<u32>> = BTreeMap::new();
    for shard in manifest
        .shards
        .iter()
        .filter(|s| !moving.contains(&s.index))
    {
        let domain = domains[shard.remote.as_str()]
            .as_deref()
            .unwrap_or_default();
        per_group
            .entry((shard.group, domain))
            .or_default()
            .push(shard.index);
    }
    for mut indexes in per_group.into_values() {
        indexes.sort_unstable();
        for index in indexes.into_iter().skip(coding.parity_shards) {
            moving.insert(index);
            excess += 1;
        }
    }
    if excess > 0 {
        reasons.push(format!(
            "{excess} shard(s) exceed {} per outage group (Resilient placement)",
            coding.parity_shards
        ));
    }
    (moving, reasons)
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
