use crate::cli::ExportArgs;
use crate::config_sync::PackageExportOutcome;
use anyhow::Result;
use std::path::Path;

pub(crate) fn run_package(rclone: &str, args: &ExportArgs) -> Result<()> {
    let outcome: PackageExportOutcome = crate::config_sync::export_package(
        &args.artifact_root,
        Path::new(rclone),
        args.rclone_config.as_deref(),
        &args.age,
        args.age_recipient.as_deref(),
    )?;
    println!(
        "exported rpool artifact: {}",
        outcome.artifact_root.display()
    );
    println!("portable config: {}", outcome.portable_path.display());
    if let Some(vault) = outcome.vault_path {
        println!("encrypted crypt vault: {}", vault.display());
    } else {
        println!("encrypted crypt vault: not required");
    }
    println!("crypt remotes: {}", outcome.crypt_remotes);
    Ok(())
}
