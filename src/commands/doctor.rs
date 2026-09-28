use crate::doctor::run_checks;
use anyhow::{bail, Result};

pub(crate) fn doctor(rclone: &str, json: bool, local_only: bool) -> Result<()> {
    let reports = if local_only {
        crate::doctor::run_checks_with(None, None)
    } else {
        run_checks(rclone)
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
