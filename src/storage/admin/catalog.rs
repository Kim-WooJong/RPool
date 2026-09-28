//! Sanitized configuration facts: aliases are not proof of independent accounts.
use crate::models::volume::{CapacityDomainId, FailureDomainId};
use crate::prelude::*;
#[derive(Debug, Clone)]
struct Entry {
    kind: String,
    backing: Option<String>,
    encrypted: bool,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct RemoteCatalog {
    entries: BTreeMap<String, Entry>,
}
#[derive(Debug, Clone)]
pub(crate) struct CapacityBinding {
    pub(crate) target: String,
    pub(crate) domain: Option<CapacityDomainId>,
    // Config section identity alone cannot prove an independent outage domain.
    pub(crate) failure_domain: Option<FailureDomainId>,
}
impl RemoteCatalog {
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
        Ok(Self { entries })
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
            // These backends report account-wide quota; paths share that quota.
            "drive" | "onedrive" | "dropbox" | "box" | "pcloud" => Ok(CapacityBinding {
                target: format!("{name}:"),
                domain: Some(CapacityDomainId::new(format!("rclone-alias-group:{name}"))?),
                failure_domain: None,
            }),
            _ => Ok(CapacityBinding {
                target: raw.to_owned(),
                domain: None,
                failure_domain: None,
            }),
        }
    }
    pub(crate) fn capacity_remotes(&self, remotes: &[String]) -> Result<Vec<String>> {
        let mut seen = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for remote in remotes {
            let b = self.capacity(remote)?;
            let identity = b
                .domain
                .as_ref()
                .map(|id| format!("domain:{}", id.as_str()))
                .unwrap_or_else(|| format!("target:{}", b.target));
            if seen.insert(identity) {
                targets.insert(b.target);
            }
        }
        Ok(targets.into_iter().collect())
    }
}
