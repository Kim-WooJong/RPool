//! `rpool pool compact`: checkpoint the pool's metadata; mark/delete covered
//! records once deletion is enabled. `--json` prints the report.
use crate::mount::metadata_compaction::Report;
use anyhow::Result;

/// Compacts the pool's drive metadata via `mount::metadata_pool::compact_pool`
/// and prints the report as JSON or text. Called via `commands::pool::compact`.
pub(crate) fn run(
    rclone: &str,
    name: &str,
    dry_run: bool,
    enable_deletion: bool,
    json: bool,
) -> Result<()> {
    let report = crate::mount::metadata_pool::compact_pool(rclone, name, dry_run, enable_deletion)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    match report {
        None => println!("pool '{name}' has no drive metadata yet; nothing to compact"),
        Some(report) => print!("{}", text(&report)),
    }
    Ok(())
}

/// Human-readable summary of a compaction report; verbs switch to "would …" on a dry run.
fn text(r: &Report) -> String {
    let verb = |done: &str, would: &str| {
        if r.dry_run {
            would.to_string()
        } else {
            done.to_string()
        }
    };
    let mut out = format!(
        "{} metadata: {} records ({} KiB), {} checkpoint(s) in {} chunk(s), {} covered, {} not yet checkpointed\n",
        r.family,
        r.records,
        r.record_bytes / 1024,
        r.checkpoints,
        r.chunks,
        r.covered,
        r.uncovered
    );
    if r.checkpointed_records > 0 {
        out += &format!(
            "{} {} records\n",
            verb("checkpointed", "would checkpoint"),
            r.checkpointed_records
        );
    }
    out += &format!(
        "deletion: {}; marks {}, {} {} newly, {} deletable, {} {}\n",
        if r.deletion_enabled { "enabled" } else { "off" },
        r.marks,
        r.marked,
        verb("marked", "would mark"),
        r.deletable,
        r.deleted,
        verb("deleted", "would be deleted")
    );
    if let Some(next) = r.next_deletion_unix {
        out += &format!("next deletion possible at unix {next}\n");
    }
    if r.broken_checkpoints > 0 {
        out += &format!(
            "warning: {} unusable checkpoint(s) ignored\n",
            r.broken_checkpoints
        );
    }
    for note in &r.notes {
        out += &format!("note: {note}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dry_run_text_says_would() {
        let report = Report {
            family: "v6".into(),
            dry_run: true,
            records: 3,
            checkpointed_records: 3,
            uncovered: 3,
            ..Default::default()
        };
        let out = text(&report);
        assert!(out.contains("would checkpoint 3 records"));
        assert!(out.contains("deletion: off"));
    }
    #[test]
    fn compact_command_parses() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "rpool",
            "pool",
            "compact",
            "p",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::cli::Commands::Pool(crate::cli::PoolArgs {
                command: crate::cli::PoolCommands::Compact {
                    dry_run: true,
                    json: true,
                    enable_deletion: false,
                    ..
                }
            }))
        ));
    }
}
