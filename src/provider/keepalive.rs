//! "Keep alive": one cheap authenticated call per account (`rclone about`,
//! or a one-level folder listing when the backend has no `about`), recorded
//! as activity. Providers decide what counts as account activity; RPool
//! cannot guarantee that API access prevents an inactivity deletion.
use crate::prelude::*;
use crate::storage::account::inactivity::keepalive_due;
use crate::storage::account::ledger::Ledger;
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::OperationContext;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Upper bound for each single keep-alive rclone call.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Result of one keep-alive attempt, printed by the CLI and shown in the GUI.
pub(crate) struct KeepaliveOutcome {
    /// Account (remote name without the trailing colon) that was contacted.
    pub account: String,
    /// Whether the call succeeded (and activity was recorded in the ledger).
    pub ok: bool,
    /// `about` or `list`.
    pub method: Option<&'static str>,
    /// rclone error text when the call failed; `None` on success.
    pub error: Option<String>,
}

/// Runs the keep-alive call against `account` (a remote name).
/// `cancel` stops it early (unmount).
pub(crate) fn keepalive_one(
    context: &RcloneContext,
    ledger: Option<&Ledger>,
    account: &str,
    cancel: Option<&Arc<AtomicBool>>,
) -> KeepaliveOutcome {
    let name = account.trim_end_matches(':');
    let root = format!("{name}:");
    let ctx = || {
        let deadline = Instant::now() + CALL_TIMEOUT;
        match cancel {
            Some(flag) => OperationContext::with_deadline_and_cancel(deadline, flag.clone()),
            None => OperationContext::with_deadline(deadline),
        }
    };
    let about = context.capture(&ctx(), &["about", "--json", "--", &root]);
    let result = match about {
        Ok(_) => Ok("about"),
        Err(_) => context
            .capture(
                &ctx(),
                &[
                    "lsjson",
                    "--max-depth",
                    "1",
                    "--dirs-only",
                    "--no-mimetype",
                    "--",
                    &root,
                ],
            )
            .map(|_| "list"),
    };
    match result {
        Ok(method) => {
            if let Some(ledger) = ledger {
                let now = crate::utils::now_unix();
                let _ = ledger.update(|data| {
                    data.note_activity(name, now);
                    data.account(name).last_keepalive = Some(now);
                });
            }
            KeepaliveOutcome {
                account: name.to_owned(),
                ok: true,
                method: Some(method),
                error: None,
            }
        }
        Err(error) => KeepaliveOutcome {
            account: name.to_owned(),
            ok: false,
            method: None,
            error: Some(error.to_string()),
        },
    }
}

/// Keep-alive for each of `accounts`, one after another.
pub(crate) fn keepalive(rclone: &str, accounts: &[String]) -> Vec<KeepaliveOutcome> {
    let context = RcloneContext::inherited(rclone);
    let ledger = crate::storage::account::runtime::ledger();
    accounts
        .iter()
        .map(|account| keepalive_one(&context, ledger.as_ref(), account, None))
        .collect()
}

/// Accounts behind `remotes` (pool addresses) whose last activity from this
/// computer is older than `every_days`, with the names their activity is
/// recorded under.
pub(crate) fn due_accounts(
    config: &Value,
    remotes: &[String],
    activity: &crate::storage::account::ledger::LedgerData,
    every_days: u32,
    now: u64,
) -> Vec<String> {
    let mut chains: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for remote in remotes {
        let Ok(name) = crate::storage::rclone::remote_name(remote) else {
            continue;
        };
        let (account, _) = crate::storage::rclone::write_account(config, name);
        chains.entry(account).or_default().push(name.to_owned());
    }
    chains
        .into_iter()
        .filter(|(account, names)| {
            let names = std::iter::once(account.as_str()).chain(names.iter().map(String::as_str));
            keepalive_due(activity.last_activity(names), every_days, now)
        })
        .map(|(account, _)| account)
        .collect()
}

/// Automatic keep-alive of a mount: accounts of `remotes` idle for the
/// configured interval get one call. Returns how many were kept alive.
pub(crate) fn run_due(rclone: &str, remotes: &[String], cancel: &Arc<AtomicBool>) -> usize {
    let Some(ledger) = crate::storage::account::runtime::ledger() else {
        return 0;
    };
    let every = crate::storage::account::runtime::settings()
        .store
        .keepalive_days;
    if every == 0 {
        return 0;
    }
    let context = RcloneContext::inherited(rclone);
    let ctx =
        OperationContext::with_deadline_and_cancel(Instant::now() + CALL_TIMEOUT, cancel.clone());
    let (Ok(config), Ok(data)) = (context.config_dump(&ctx), ledger.load()) else {
        return 0;
    };
    let due = due_accounts(&config, remotes, &data, every, crate::utils::now_unix());
    due.iter()
        .map(|account| keepalive_one(&context, Some(&ledger), account, Some(cancel)))
        .filter(|outcome| outcome.ok)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::account::ledger::LedgerData;

    #[test]
    fn due_accounts_group_crypts_by_account_and_use_the_latest_activity() {
        let config = serde_json::json!({
            "gd": {"type": "drive"},
            "gd_a": {"type": "crypt", "remote": "gd:a"},
            "gd_b": {"type": "crypt", "remote": "gd:b"},
            "od": {"type": "onedrive"},
            "od_c": {"type": "crypt", "remote": "od:"}
        });
        let now = 100 * 86_400;
        let mut data = LedgerData::default();
        data.note_activity("gd_b", now - 86_400); // one crypt used yesterday
        data.note_activity("od", now - 30 * 86_400);
        let remotes = [
            "gd_a:pool".to_string(),
            "gd_b:pool".into(),
            "od_c:pool".into(),
            "bad".into(),
        ];
        assert_eq!(due_accounts(&config, &remotes, &data, 7, now), ["od"]);
        assert!(due_accounts(&config, &remotes, &data, 0, now).is_empty());
        assert_eq!(
            due_accounts(&config, &remotes, &LedgerData::default(), 7, now),
            ["gd", "od"]
        );
    }
}
