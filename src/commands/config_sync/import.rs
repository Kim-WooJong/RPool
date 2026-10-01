use crate::cli::ImportArgs;
use crate::config_sync::{PackageCryptOutcome, PackageImportOutcome};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run_package(rclone: &str, args: &ImportArgs) -> Result<()> {
    let outcome: PackageImportOutcome = crate::config_sync::import_package(
        &args.artifact_root,
        Path::new(rclone),
        args.rclone_config.as_deref(),
        &args.age,
        args.age_identity.as_deref(),
        args.age_recipient.as_deref(),
        &args.age_keygen,
        args.dry_run,
    )?;

    if outcome.dry_run {
        println!(
            "rpool artifact validation passed: {}",
            outcome.artifact_root.display()
        );
    } else {
        println!(
            "imported rpool artifact: {}",
            outcome.artifact_root.display()
        );
    }
    println!("portable config: {}", outcome.portable_path.display());
    println!("crypt remotes: {}", outcome.crypt_remotes);
    println!(
        "crypt restore: {}",
        match outcome.crypt {
            PackageCryptOutcome::NotPresent => "not required",
            PackageCryptOutcome::AlreadyExact => "already exact",
            PackageCryptOutcome::Restored if outcome.dry_run => "restore required (dry-run only)",
            PackageCryptOutcome::Restored => "restored and verified",
            PackageCryptOutcome::RestoredWithCleanupPending => "restored; recovery cleanup pending",
        }
    );
    Ok(())
}
