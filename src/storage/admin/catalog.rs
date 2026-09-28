//! Sanitized configuration facts: aliases are not proof of independent accounts.
use crate::models::volume::{CapacityDomainId, FailureDomainId};
use crate::prelude::*;
#[derive(Debug, Clone)]
struct Entry {
    kind: String,
    backing: Option<String>,
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
pub(crate) struct RemoteCatalog {
    entries: BTreeMap<String, Entry>,
    domains: super::domains::DomainStore,
}
#[derive(Debug, Clone)]
pub(crate) struct CapacityBinding {
    pub(crate) target: String,
    pub(crate) domain: Option<CapacityDomainId>,
    // Config section identity alone cannot prove an independent outage domain.
    pub(crate) failure_domain: Option<FailureDomainId>,
}
impl RemoteCatalog {
    pub(crate) fn set_domains(&mut self, domains: super::domains::DomainStore) {
        self.domains = domains;
    }
    pub(crate) fn identity_declared(&self, raw: &str) -> bool {
        self.placement_target(raw)
            .is_ok_and(|name| self.domains.remotes.contains_key(&name))
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
    pub(crate) fn crypt_remotes(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, e)| e.kind == "crypt" && e.encrypted)
            .map(|(n, _)| format!("{n}:"))
            .collect()
    }
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
    pub(crate) fn capacity(&self, raw: &str) -> Result<CapacityBinding> {
        self.resolve(raw, &mut BTreeSet::new())
    }
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
                let binding = self.resolve(
                    entry
                        .backing
                        .as_deref()
                        .ok_or_else(|| anyhow!("missing capacity backing"))?,
                    visited,
                )?;
                if binding.domain.is_none() {
                    bail!("alias capacity scope is unresolved for this backend");
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
