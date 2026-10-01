use crate::models::{PortableConfig, CONFIG_SYNC_FORMAT, CONFIG_SYNC_VERSION};
use crate::pool::{validate_pool, validate_pool_name};
use anyhow::{bail, Result};

pub(crate) fn validate_bundle(bundle: &PortableConfig) -> Result<()> {
    if bundle.format != CONFIG_SYNC_FORMAT {
        bail!("unsupported config bundle format: {}", bundle.format);
    }
    if bundle.version != CONFIG_SYNC_VERSION {
        bail!("unsupported config bundle version: {}", bundle.version);
    }
    if bundle.pools.version != 1 {
        bail!(
            "unsupported pool config version in bundle: {}",
            bundle.pools.version
        );
    }
    if bundle.remote_roots.version != 1 {
        bail!(
            "unsupported remote-root config version in bundle: {}",
            bundle.remote_roots.version
        );
    }

    if let Some(limits) = &bundle.account_limits {
        limits.validate()?;
    }
    if let Some(binding) = &bundle.secret_vault {
        binding.validate()?;
        if bundle.crypt_remotes.is_empty() {
            bail!("portable config has a crypt secret vault binding but no crypt remotes");
        }
    }

    let mut crypt_names = std::collections::BTreeSet::new();
    for remote in &bundle.crypt_remotes {
        remote.validate_structure()?;
        if !crypt_names.insert(&remote.name) {
            bail!("duplicate portable crypt remote");
        }
    }

    for (name, pool) in &bundle.pools.pools {
        validate_pool_name(name)?;
        validate_pool(pool)?;
    }

    if let Some(encryption) = &bundle.gui.encryption {
        encryption.validate()?;
    }
    if bundle.gui.shard_mib == 0 {
        bail!("portable GUI shard_mib must be greater than zero");
    }
    if bundle.gui.workers == 0 {
        bail!("portable GUI workers must be greater than zero");
    }
    if bundle.gui.parity_shards > 0 {
        crate::erasure::validate_rs_counts(bundle.gui.data_shards, bundle.gui.parity_shards)?;
    }

    Ok(())
}
