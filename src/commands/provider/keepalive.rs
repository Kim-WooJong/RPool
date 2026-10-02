//! `rpool provider keepalive`.
use crate::prelude::*;
use crate::provider::keepalive::keepalive;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

/// Makes one cheap authenticated call per backing account (the given remotes
/// resolved to their accounts, or every account) and records it as activity;
/// fails if any account did not answer.
pub(crate) fn run(rclone: &str, remotes: Vec<String>, json: bool) -> Result<()> {
    let accounts = if remotes.is_empty() {
        RcloneAdmin::inherited(rclone).catalog()?.backing_remotes()
    } else {
        let context = crate::storage::rclone::RcloneContext::inherited(rclone);
        let ctx = crate::storage::traits::OperationContext::with_deadline(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        let config = context.config_dump(&ctx)?;
        let mut accounts: Vec<String> = remotes
            .iter()
            .map(|remote| {
                let name = remote.trim().trim_end_matches(':');
                let name = name.split(':').next().unwrap_or(name);
                crate::storage::rclone::write_account(&config, name).0
            })
            .collect();
        accounts.sort();
        accounts.dedup();
        accounts
    };
    if accounts.is_empty() {
        bail!("no accounts are configured in rclone");
    }
    let outcomes = keepalive(rclone, &accounts);
    if json {
        println!("{}", serde_json::to_string_pretty(&outcomes)?);
    } else {
        for outcome in &outcomes {
            match (&outcome.method, &outcome.error) {
                (Some(method), _) => println!("{:<24} ok ({method})", outcome.account),
                (None, Some(error)) => println!("{:<24} FAILED: {error}", outcome.account),
                _ => {}
            }
        }
        println!("Recorded as activity. Providers decide what counts as account activity; RPool cannot guarantee that API access keeps an inactive account.");
    }
    if outcomes.iter().any(|o| !o.ok) {
        bail!("some accounts did not answer");
    }
    Ok(())
}
