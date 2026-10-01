use crate::doctor::run_checks;
use anyhow::{bail, Result};
use std::path::Path;

pub(crate) fn doctor(
    rclone: &str,
    json: bool,
    local_only: bool,
    bundle: Option<&Path>,
) -> Result<()> {
    if let Some(output) = bundle {
        return export_bundle(rclone, output, json, local_only);
    }
    let reports = if local_only {
        crate::doctor::run_checks_with(None, None)
    } else {
        let mut reports = run_checks(rclone);
        reports.extend(crate::doctor::check_metadata(rclone));
        reports
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        for report in &reports {
            println!(
                "{:<5} {:<20} {}",
                report.status.to_uppercase(),
                report.check,
                report.message
            );
        }
    }

    if reports.iter().any(|report| report.status == "fail") {
        bail!("doctor found one or more failures");
    }
    Ok(())
}

/// A bundle is for reporting problems, so doctor failures do not fail it.
fn export_bundle(rclone: &str, output: &Path, json: bool, local_only: bool) -> Result<()> {
    let summary = crate::doctor::bundle::export(rclone, output, local_only)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    println!(
        "Diagnostics bundle written: {} ({} files, {} bytes)",
        summary.path.display(),
        summary.files.len(),
        summary.bytes
    );
    for file in &summary.files {
        println!("  {file}");
    }
    if !summary.skipped.is_empty() {
        println!("Not included:");
        for skipped in &summary.skipped {
            println!("  {skipped}");
        }
    }
    println!("Secrets were removed (see manifest.txt); review the files before sharing.");
    Ok(())
}
