//! Non-secret, user-declared accounting and outage identities.
use crate::prelude::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DomainIdentity {
    pub capacity: String,
    pub failure: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DomainStore {
    pub version: u32,
    pub remotes: BTreeMap<String, DomainIdentity>,
}
impl Default for DomainStore {
    fn default() -> Self {
        Self {
            version: 1,
            remotes: BTreeMap::new(),
        }
    }
}
impl DomainStore {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported provider domain store");
        }
        for (remote, identity) in &self.remotes {
            if remote.is_empty() || remote.contains(':') || remote.chars().any(char::is_control) {
                bail!("identity key must be a backing remote name, not a path");
            }
            crate::models::volume::CapacityDomainId::new(identity.capacity.clone())?;
            if !identity.failure.is_empty() {
                crate::models::volume::FailureDomainId::new(identity.failure.clone())?;
            }
        }
        Ok(())
    }
    pub(crate) fn load() -> Result<Self> {
        let path = crate::config::app_config_dir()?.join("provider_domains.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let store: Self = crate::utils::read_json(&path)?;
        store.validate()?;
        Ok(store)
    }
    pub(crate) fn save(&self) -> Result<()> {
        self.validate()?;
        let dir = crate::config::app_config_dir()?;
        fs::create_dir_all(&dir)?;
        crate::utils::save_json_atomic(&dir.join("provider_domains.json"), self)
    }
}

pub(crate) fn update(capacity: &[String], failure: &[String]) -> Result<()> {
    if capacity.is_empty() && failure.is_empty() {
        return Ok(());
    }
    let mut store = DomainStore::load()?;
    for (items, is_capacity) in [(capacity, true), (failure, false)] {
        for item in items {
            let (remote, id) = item
                .split_once('=')
                .context("domain mapping must be backing-remote=identity")?;
            let entry = store.remotes.entry(remote.trim().to_owned()).or_default();
            if is_capacity {
                entry.capacity = id.trim().into();
            } else {
                entry.failure = id.trim().into();
            }
        }
    }
    store.save()
}
