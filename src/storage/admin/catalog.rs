//! Sanitized configuration facts: aliases are not proof of independent accounts.
use crate::models::volume::{CapacityDomainId, FailureDomainId};
use crate::prelude::*;
/// Only filesystem backends whose quota may describe a selected disk root.
pub(crate) fn path_capacity_supported(kind: &str) -> bool {
    matches!(kind, "sftp" | "smb" | "local")
}
/// Lexical quota identity only: never use this case-folded path for I/O.
/// The explicit flag lets non-Windows tests exercise Windows local semantics.
pub(super) fn quota_path_key(path: &str, kind: &str, windows: bool) -> String {
    let windows_local = windows && kind == "local";
    let path = if windows_local {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let mut rest = path.as_str();
    let mut prefix = String::new();
    let mut anchored = false;
    let unc_tail;
    if windows_local
        && rest.as_bytes().get(1) == Some(&b':')
        && rest.as_bytes()[0].is_ascii_alphabetic()
    {
        prefix.push_str(&rest[..2]);
        rest = &rest[2..];
        if rest.starts_with('/') {
            prefix.push('/');
            rest = rest.trim_start_matches('/');
            anchored = true;
        }
    } else if windows_local && rest.starts_with("//") {
        // A UNC server/share is an indivisible root; '..' cannot remove it.
        let mut components = rest[2..].split('/').filter(|p| !p.is_empty());
        prefix = format!("//{}", components.next().unwrap_or_default());
        if let Some(share) = components.next() {
            prefix.push('/');
            prefix.push_str(share);
        }
        unc_tail = components.collect::<Vec<_>>().join("/");
        rest = &unc_tail;
        prefix.push('/');
        anchored = true;
    } else if rest.starts_with('/') {
        anchored = true;
        // SMB treats leading slashes as share-root separators, not a
        // different namespace. POSIX and Windows rooted paths retain '/'.
        if kind != "smb" {
            prefix.push('/');
        }
        rest = rest.trim_start_matches('/');
    }
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if anchored => {}
            _ => parts.push(part),
        }
    }
    let mut key = format!("{prefix}{}", parts.join("/"));
    if windows_local && key.starts_with("//") {
        key.truncate(key.trim_end_matches('/').len());
    }
    if windows_local || kind == "smb" {
        key.to_lowercase()
    } else {
        key
    }
}
#[derive(Debug, Clone)]
/// One rclone config section, reduced to non-secret facts.
struct Entry {
    /// Backend type in lowercase (`crypt`, `alias`, `drive`, …).
    kind: String,
    /// The section's `remote =` value for wrappers (e.g. `base:folder`).
    backing: Option<String>,
    /// For crypt: data encryption is on (`no_data_encryption` unset or false).
    encrypted: bool,
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    #[test]
    fn wrappers_collapse_and_unknown_or_aggregate_targets_fail_closed() {
        let catalog = RemoteCatalog::parse(&serde_json::json!({
            "base": {"type": "s3"}, "alias": {"type": "alias", "remote": "base:bucket"},
            "crypt": {"type": "crypt", "remote": "alias:folder"},
            "aggregate": {"type": "union"}, "cycle": {"type": "alias", "remote": "cycle:"}
        }))
        .unwrap();
        assert_eq!(catalog.placement_target("crypt:files").unwrap(), "base");
        for raw in ["aggregate:", "cycle:", "missing:"] {
            assert!(catalog.placement_target(raw).is_err());
        }
    }
}
#[derive(Debug, Clone, Default)]
/// Sanitized view of the rclone config (from `config dump`) plus the
/// user-declared domain identities. Built by `RcloneAdmin::catalog`; used for
/// placement, capacity budgets, usage and encryption checks.
pub(crate) struct RemoteCatalog {
    /// Config sections by name.
    entries: BTreeMap<String, Entry>,
    /// Declared capacity/failure identities from `provider_domains.json`.
    domains: super::domains::DomainStore,
}
#[derive(Debug, Clone)]
/// Where one remote's quota is queried and which domains it belongs to.
pub(crate) struct CapacityBinding {
    /// Address passed to `rclone about` (account root for whole-account backends).
    pub(crate) target: String,
    /// Capacity domain; undeclared backends share `unverified-accounts`.
    pub(crate) domain: Option<CapacityDomainId>,
    // Config section identity alone cannot prove an independent outage domain.
    /// Declared outage domain, if any.
    pub(crate) failure_domain: Option<FailureDomainId>,
}
impl RemoteCatalog {
    /// Replaces the declared domain identities (loaded by `RcloneAdmin::catalog`).
    pub(crate) fn set_domains(&mut self, domains: super::domains::DomainStore) {
        self.domains = domains;
    }
    /// Whether the backing section of `raw` has a declared identity.
    pub(crate) fn identity_declared(&self, raw: &str) -> bool {
        if self.capacity(raw).is_err() {
            return false;
        }
        let mut address = raw;
        let mut seen = BTreeSet::new();
        while let Some((name, _)) = address.split_once(':') {
            if !seen.insert(name) {
                return false;
            }
            if self.domains.remotes.contains_key(name) {
                return true;
            }
            let Some(entry) = self.entries.get(name) else {
                return false;
            };
            if !matches!(entry.kind.as_str(), "crypt" | "alias" | "chunk" | "chunker") {
                return false;
            }
            let Some(next) = entry.backing.as_deref() else {
                return false;
            };
            address = next;
        }
        false
    }
    /// Non-secret configured backing of a crypt location, for GUI display.
    pub(crate) fn crypt_location(&self, raw: &str) -> Option<&str> {
        let name = raw.split_once(':')?.0;
        let entry = self.entries.get(name)?;
        (entry.kind == "crypt")
            .then_some(entry.backing.as_deref())
            .flatten()
    }
    /// Conservative lexical key for duplicate quota targets, not a server realpath.
    pub(crate) fn quota_scope_key(&self, raw: &str) -> String {
        let Some((name, path)) = raw.split_once(':') else {
            return raw.into();
        };
        let path = quota_path_key(
            path,
            self.backend_kind(name).unwrap_or_default(),
            cfg!(windows),
        );
        format!("{name}:{path}")
    }

    /// The configured backend type of a section (for example `dropbox`, `s3`).
    pub(crate) fn backend_kind(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|entry| entry.kind.as_str())
    }

    /// Resolve known wrappers to one configured backing section. This collapses
    /// known aliases, but is NOT proof of independent accounts/providers.
    pub(crate) fn placement_target(&self, raw: &str) -> Result<String> {
        let mut address = raw;
        let mut seen = BTreeSet::new();
        loop {
            let (name, _) = address
                .split_once(':')
                .ok_or_else(|| anyhow!("invalid placement address"))?;
            if !seen.insert(name) {
                bail!("placement alias cycle");
            }
            let entry = self
                .entries
                .get(name)
                .ok_or_else(|| anyhow!("unresolved placement target"))?;
            match entry.kind.as_str() {
                "crypt" | "alias" | "chunk" | "chunker" => {
                    address = entry
                        .backing
                        .as_deref()
                        .ok_or_else(|| anyhow!("missing placement backing"))?;
                }
                "union" | "combine" => {
                    bail!("aggregate placement target cannot establish shard isolation")
                }
                _ => return Ok(name.to_owned()),
            }
        }
    }
    /// Builds the catalog from rclone `config dump` JSON; sections without a
    /// `type` are skipped. Domains start empty.
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("invalid remote catalog"))?;
        let mut entries = BTreeMap::new();
        for (name, raw) in object {
            if let Some(kind) = raw.get("type").and_then(Value::as_str) {
                let encrypted = match raw.get("no_data_encryption") {
                    None | Some(Value::Bool(false)) => true,
                    Some(Value::String(s)) => matches!(
                        s.trim().to_ascii_lowercase().as_str(),
                        "false" | "0" | "no" | "off"
                    ),
                    _ => false,
                };
                entries.insert(
                    name.clone(),
                    Entry {
                        kind: kind.to_ascii_lowercase(),
                        backing: raw.get("remote").and_then(Value::as_str).map(str::to_owned),
                        encrypted,
                    },
                );
            }
        }
        Ok(Self {
            entries,
            domains: Default::default(),
        })
    }
    /// Physical/base providers not covered by an encrypted crypt remote.
    /// Wrapper chains are followed without treating folder suffixes as names.
    pub(crate) fn missing_encryption_remotes(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(provider, entry)| {
                !entry.kind.is_empty()
                    && !matches!(
                        entry.kind.as_str(),
                        "crypt" | "alias" | "chunk" | "chunker" | "union" | "combine"
                    )
                    && !self.entries.values().any(|crypt| {
                        if crypt.kind != "crypt" || !crypt.encrypted {
                            return false;
                        }
                        let mut address = crypt.backing.as_deref();
                        let mut visited = BTreeSet::new();
                        while let Some(raw) = address {
                            let Some((name, _)) = raw.split_once(':') else {
                                return false;
                            };
                            if name == provider.as_str() {
                                return true;
                            }
                            if !visited.insert(name) {
                                return false;
                            }
                            let Some(next) = self.entries.get(name) else {
                                return false;
                            };
                            if !matches!(
                                next.kind.as_str(),
                                "alias" | "chunk" | "chunker" | "crypt"
                            ) {
                                return false;
                            }
                            address = next.backing.as_deref();
                        }
                        false
                    })
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Names of concrete (non-wrapper, non-aggregate) sections, without colon.
    /// Used by limits view, keep-alive and usage refresh.
    pub(crate) fn backing_remotes(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, e)| {
                !matches!(
                    e.kind.as_str(),
                    "crypt" | "alias" | "chunk" | "chunker" | "union" | "combine"
                )
            })
            .map(|(n, _)| n.clone())
            .collect()
    }
    /// Encrypted crypt remotes as `name:` addresses.
    pub(crate) fn crypt_remotes(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, e)| e.kind == "crypt" && e.encrypted)
            .map(|(n, _)| format!("{n}:"))
            .collect()
    }
    /// Concrete sections as `name:` addresses with the configured remote root applied.
    pub(crate) fn physical_remotes(&self) -> Result<Vec<String>> {
        self.entries
            .iter()
            .filter(|(_, e)| {
                !matches!(
                    e.kind.as_str(),
                    "crypt" | "alias" | "chunk" | "chunker" | "union" | "combine"
                )
            })
            .map(|(n, _)| crate::remote_root::apply_remote_root(&format!("{n}:")))
            .collect()
    }
    /// Capacity binding of address `raw`, following wrapper chains; errors on
    /// cycles, unknown sections, aggregates and unresolved alias scopes.
    pub(crate) fn capacity(&self, raw: &str) -> Result<CapacityBinding> {
        self.resolve(raw, &mut BTreeSet::new())
    }
    /// Recursive step of [`Self::capacity`]; `visited` detects alias cycles.
    fn resolve(&self, raw: &str, visited: &mut BTreeSet<String>) -> Result<CapacityBinding> {
        let parsed = super::super::reference::LegacyAddress::parse(raw)
            .map_err(|_| anyhow!("invalid legacy capacity address"))?;
        let name = parsed.remote();
        if !visited.insert(name.to_owned()) {
            bail!("capacity alias cycle");
        }
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| anyhow!("capacity mapping is unresolved"))?;
        match entry.kind.as_str() {
            "crypt" | "alias" | "chunk" | "chunker" => {
                let backing = entry
                    .backing
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing capacity backing"))?;
                let joined;
                let next = if entry.kind == "alias" {
                    // Alias forwards its plaintext suffix unchanged. Crypt does
                    // not: without its cipher we can only trust its backing root.
                    let suffix = raw.split_once(':').unwrap().1.trim_start_matches('/');
                    let backing_path = backing.split_once(':').map(|(_, p)| p);
                    if !suffix.is_empty() {
                        let separator =
                            if backing_path.is_some_and(|p| p.is_empty() || p.ends_with('/')) {
                                ""
                            } else {
                                "/"
                            };
                        joined = format!("{backing}{separator}{suffix}");
                        joined.as_str()
                    } else {
                        backing
                    }
                } else {
                    backing
                };
                let mut binding = self.resolve(next, visited)?;
                if binding.domain.is_none() {
                    bail!("alias capacity scope is unresolved for this backend");
                }
                if let Some(identity) = self.domains.remotes.get(name) {
                    let base = self.placement_target(raw)?;
                    if !path_capacity_supported(self.backend_kind(&base).unwrap_or_default()) {
                        bail!("per-location capacity identity requires a filesystem backend; declare shared account quota on the backing provider");
                    }
                    binding.domain = Some(CapacityDomainId::new(identity.capacity.clone())?);
                    // A disk does not become a new server/account outage domain.
                    // Always keep the underlying provider's failure identity.
                }
                Ok(binding)
            }
            "union" | "combine" => bail!("aggregate remote capacity is unresolved"),
            _ => {
                let identity = self.domains.remotes.get(name);
                // Query every concrete backend; about support is a runtime fact,
                // not a provider allowlist. Undeclared identities share a
                // conservative overlap group rather than pretending independence.
                let domain = identity
                    .map(|i| i.capacity.clone())
                    .unwrap_or_else(|| "unverified-accounts".into());
                Ok(CapacityBinding {
                    target: if matches!(
                        entry.kind.as_str(),
                        "drive" | "onedrive" | "dropbox" | "box" | "pcloud"
                    ) {
                        format!("{name}:")
                    } else {
                        raw.to_owned()
                    },
                    domain: Some(CapacityDomainId::new(domain)?),
                    failure_domain: identity
                        .filter(|i| !i.failure.is_empty())
                        .map(|i| FailureDomainId::new(i.failure.clone()))
                        .transpose()?,
                })
            }
        }
    }
    /// Distinct quota targets of `remotes`, sorted; used by the GUI usage refresh.
    pub(crate) fn capacity_remotes(&self, remotes: &[String]) -> Result<Vec<String>> {
        let mut seen = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for remote in remotes {
            let b = self.capacity(remote)?;
            let identity = b.target.clone();
            if seen.insert(identity) {
                targets.insert(b.target);
            }
        }
        Ok(targets.into_iter().collect())
    }
}
