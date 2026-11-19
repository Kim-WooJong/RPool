//! Shared capacity-only edits for the GUI and CLI. Never changes rclone config,
//! account options, outage metadata, pools, keys, plans or file bytes.
use super::catalog::RemoteCatalog;
use super::domains::DomainStore;
use crate::prelude::*;

/// Sanitized saved capacity view shared by the CLI and locations dialog.
#[derive(Debug, Serialize)]
pub(crate) struct LocationCapacity {
    /// Exact crypt section name.
    pub(crate) remote: String,
    /// Concrete quota probe address, not an I/O path rewrite.
    pub(crate) backing: String,
    /// Explicit independent disk or provider inheritance.
    pub(crate) mode: &'static str,
    /// Only the crypt capacity override, if present.
    pub(crate) declared_group: Option<String>,
    /// Resolved saved capacity group after inheritance.
    pub(crate) effective_group: String,
    /// Configured crypts resolving to the same capacity group.
    pub(crate) same_group_locations: Vec<String>,
    /// Effective outage group of the backing provider.
    pub(crate) failure_group: Option<String>,
    /// Declarations are local and never exported.
    pub(crate) pc_local: bool,
    /// Applicability boundary for new jobs and immutable plans.
    pub(crate) applies_to: &'static str,
}

/// Read-only view; does not save, create directories, query quota or mount.
pub(crate) fn describe(
    catalog: &RemoteCatalog, store: &DomainStore, name: &str,
) -> Result<LocationCapacity> {
    let mut catalog = catalog.clone();
    catalog.set_domains(store.clone());
    let binding = catalog.editable_capacity_location(name)?;
    let effective_group = binding.domain.context("unresolved capacity group")?.as_str().to_owned();
    let same_group_locations = catalog.capacity_locations().into_iter().filter(|raw| {
        catalog.capacity(raw).ok().and_then(|b| b.domain)
            .is_some_and(|d| d.as_str() == effective_group)
    }).collect();
    let declared_group = store.remotes.get(name).filter(|i| !i.capacity.is_empty())
        .map(|i| i.capacity.clone());
    Ok(LocationCapacity {
        remote: name.into(), backing: binding.target,
        mode: if declared_group.is_some() { "independent-disk" } else { "inherit-provider" },
        declared_group, effective_group, same_group_locations,
        failure_group: binding.failure_domain.map(|d| d.as_str().to_owned()),
        pc_local: true, applies_to: "new jobs and newly calculated plans only; existing immutable plans are unchanged",
    })
}

/// Merge only dirty rows into the newest store, with optimistic row checks.
/// None = inherit; Some = explicit independent-disk declaration.
pub(crate) fn merged(
    mut live: DomainStore, before: &DomainStore,
    catalog: &RemoteCatalog, original_catalog: &RemoteCatalog,
    edits: &BTreeMap<String, Option<String>>,
) -> Result<DomainStore> {
    if edits.is_empty() { return Ok(live); }
    for (name, group) in edits {
        catalog.editable_capacity_location(name)?;
        original_catalog.editable_capacity_location(name)?;
        let base = catalog.placement_target(&format!("{name}:"))?;
        let original_base = original_catalog.placement_target(&format!("{name}:"))?;
        if catalog.backend_kind(&base) != original_catalog.backend_kind(&original_base)
            || catalog.crypt_location(&format!("{name}:"))
            != original_catalog.crypt_location(&format!("{name}:"))
            || catalog.placement_target(&format!("{name}:"))?
                != original_catalog.placement_target(&format!("{name}:"))? {
            bail!("Storage location changed elsewhere; reopen Storage locations before saving");
        }
        let same = |s: &DomainStore| s.remotes.get(name).map(|i| (i.capacity.clone(), i.failure.clone()));
        // Compare owned tuples to avoid borrowing the store being updated.
        if same(&live) != same(before) {
            bail!("Capacity identities changed elsewhere; reopen Storage locations before saving");
        }
        match group {
            Some(group) => {
                let group = group.trim();
                crate::models::volume::CapacityDomainId::new(group.to_owned())?;
                anyhow::ensure!(group.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
                    "disk group ID must use ASCII letters, digits, dots, underscores or hyphens");
                // Capacity-only operation: never replace a pre-existing failure field.
                live.remotes.entry(name.clone()).or_default().capacity = group.into();
            }
            None => {
                if let Some(row) = live.remotes.get_mut(name) {
                    row.capacity.clear();
                    if row.failure.is_empty() { live.remotes.remove(name); }
                }
            }
        }
    }
    live.validate()?;
    let mut next = catalog.clone();
    next.set_domains(live.clone());
    // Reuse the admission catalog's concrete lexical quota scope key. Include
    // unchanged peers, not just the rows visible/edited in this dialog.
    let edited_scopes: BTreeSet<String> = edits.keys().map(|name| {
        next.editable_capacity_location(name).map(|b| next.quota_scope_key(&b.target))
    }).collect::<Result<_>>()?;
    let mut scopes = BTreeMap::<String, String>::new();
    for raw in next.capacity_locations() {
        // Discover roots structurally before validating declarations. A broken
        // unrelated crypt must not block every save, but matching peers (also
        // wrappers and encryption-off rows) still fail closed on capacity errors.
        let Some(scope) = next.filesystem_quota_scope(&raw) else { continue; };
        if !edited_scopes.contains(&scope) { continue; }
        let binding = next.capacity(&raw)
            .with_context(|| format!("Invalid capacity peer {raw}"))?;
        anyhow::ensure!(next.quota_scope_key(&binding.target) == scope,
            "Capacity peer root changed during validation");
        let group = binding.domain.context("unresolved location capacity")?.as_str().to_owned();
        if let Some(previous) = scopes.insert(scope.clone(), group.clone()) {
            if previous != group {
                bail!("Conflicting capacity groups on the same quota root ({scope}); assign the same group to every location on this root");
            }
        }
    }
    Ok(live)
}
