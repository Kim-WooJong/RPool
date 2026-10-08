//! Portable configuration and crypt secret portability: `rpool export` /
//! `rpool import` packages, the age-encrypted secret vault, transactional
//! restore of crypt passwords into rclone.conf, and crypt remote provisioning
//! (`provision`) used by the GUI and `rpool provider encrypt`.
/// Artifact tree layout and vault binding checks.
mod artifact;
/// Package export (`rpool export`).
mod export;
/// Package import (`rpool import`).
mod import;
/// Locating rclone.conf and deriving age recipients.
mod tooling;
/// Validation of portable config bundles.
mod validate;

pub(crate) use export::{export_package, PackageExportOutcome};
pub(crate) use import::{import_package, PackageCryptOutcome, PackageImportOutcome};
pub(crate) use validate::validate_bundle;

// Crypt Secret Portability B1-B7 implementation.
/// age encryption/decryption of the secret vault.
pub(crate) mod age_vault;
#[cfg(test)]
mod b6_integration_tests;
#[cfg(test)]
pub(crate) mod crypt_generate;
/// Restore of crypt secrets into the target rclone.conf.
pub(crate) mod crypt_restore;
/// Extraction of crypt definitions and secrets from `rclone config dump`.
pub(crate) mod crypt_secrets;
mod plaintext_config;
mod secret_process;
pub(crate) mod transaction;

pub(crate) mod provision;
/// Changing a provider's storage location with its crypt remotes.
pub(crate) mod relocate;
