use crate::models::{PortableConfig, PortableSecretVault, SECRET_VAULT_PATH};
use anyhow::{anyhow, bail, Result};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) const PORTABLE_CONFIG_PATH: &str = "config/portable-config.json";

pub(crate) struct ArtifactPaths {
    pub(crate) root: PathBuf,
    pub(crate) portable: PathBuf,
    pub(crate) vault: PathBuf,
}

fn ensure_directory(path: &Path, create: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                bail!("portable artifact path must be a regular directory");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|_| anyhow!("cannot create portable artifact directory"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("portable artifact directory is missing");
        }
        Err(_) => bail!("cannot inspect portable artifact directory"),
    }
    Ok(())
}

fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path).map_err(|_| anyhow!("portable artifact file is missing"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("portable artifact file must be a regular non-symlink file");
    }
    Ok(meta)
}

impl ArtifactPaths {
    pub(crate) fn for_export(root: &Path) -> Result<Self> {
        ensure_directory(root, true)?;
        let root = root.canonicalize().map_err(|_| anyhow!("cannot resolve portable artifact directory"))?;
        let config = root.join("config");
        let secrets = root.join("secrets");
        ensure_directory(&config, true)?;
        ensure_directory(&secrets, true)?;
        Ok(Self {
            portable: root.join(PORTABLE_CONFIG_PATH),
            vault: root.join(SECRET_VAULT_PATH),
            root,
        })
    }

    pub(crate) fn for_import(root: &Path) -> Result<Self> {
        ensure_directory(root, false)?;
        let root = root.canonicalize().map_err(|_| anyhow!("cannot resolve portable artifact directory"))?;
        let config = root.join("config");
        let secrets = root.join("secrets");
        ensure_directory(&config, false)?;
        ensure_directory(&secrets, false)?;
        let paths = Self {
            portable: root.join(PORTABLE_CONFIG_PATH),
            vault: root.join(SECRET_VAULT_PATH),
            root,
        };
        regular_file(&paths.portable)?;
        Ok(paths)
    }

    pub(crate) fn vault_digest(&self) -> Result<String> {
        let meta = regular_file(&self.vault)?;
        if meta.len() == 0 || meta.len() > 64 * 1024 * 1024 {
            bail!("unsupported encrypted vault size");
        }
        let mut file = File::open(&self.vault).map_err(|_| anyhow!("cannot open encrypted vault"))?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|_| anyhow!("cannot hash encrypted vault"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    pub(crate) fn validate_binding(&self, bundle: &PortableConfig) -> Result<()> {
        match (&bundle.secret_vault, bundle.crypt_remotes.is_empty()) {
            (None, true) => {
                match fs::symlink_metadata(&self.vault) {
                    Ok(_) => bail!("unexpected crypt secret vault for a portable config with no crypt remotes"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(_) => bail!("cannot inspect encrypted crypt vault path"),
                }
            }
            (Some(binding), false) => {
                binding.validate()?;
                if binding.blake3 != self.vault_digest()? {
                    bail!("portable config and encrypted crypt vault do not belong to the same export");
                }
            }
            (None, false) => bail!("portable config references crypt remotes but has no encrypted vault binding"),
            (Some(_), true) => bail!("portable config has an encrypted vault binding but no crypt remotes"),
        }
        Ok(())
    }

    pub(crate) fn remove_stale_vault(&self) -> Result<()> {
        match fs::symlink_metadata(&self.vault) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    bail!("refusing to remove unsafe stale vault path");
                }
                fs::remove_file(&self.vault).map_err(|_| anyhow!("cannot remove stale encrypted vault"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => bail!("cannot inspect stale encrypted vault"),
        }
        Ok(())
    }

    pub(crate) fn binding(&self) -> Result<PortableSecretVault> {
        PortableSecretVault::new(self.vault_digest()?)
    }
}
