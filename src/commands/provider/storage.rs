//! `rpool provider storage list|clean`.
use crate::cli::StorageCommands;
use crate::prelude::*;
use crate::provider::storage::{self, execute::Request};

/// Runs one `provider storage` subcommand and prints its report (text or
/// JSON). A postponed cleanup is an error (exit status), after the report.
pub(crate) fn run(rclone: &str, command: StorageCommands) -> Result<()> {
    match command {
        StorageCommands::List { remote, json } => {
            let view = storage::list(rclone, &remote)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                print!(
                    "{}",
                    crate::presentation::provider_storage::view_text(&view)
                );
            }
            Ok(())
        }
        StorageCommands::Clean {
            remote,
            groups,
            include_unconfirmed_generations,
            include_foreign_pools,
            confirm,
            force,
            cancel,
            workers,
            json,
        } => {
            let request = Request {
                groups,
                confirm,
                force,
                cancel,
                unconfirmed: include_unconfirmed_generations,
                foreign: include_foreign_pools,
                workers,
            };
            let report = storage::clean(rclone, &remote, &request)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!(
                    "{}",
                    crate::presentation::provider_storage::clean_text(&report)
                );
            }
            if report.mode == storage::model::CleanMode::Postponed {
                bail!("cleanup postponed: some sources could not be read");
            }
            Ok(())
        }
    }
}
