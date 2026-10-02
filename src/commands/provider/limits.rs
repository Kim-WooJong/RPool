//! `rpool provider limits …` commands.
use crate::cli::LimitsCommands;
use crate::prelude::*;
use crate::presentation::format_bytes;
use crate::provider::limits_view::{self, AccountStatus};
use crate::storage::account::budget::wait_text;
use crate::storage::account::edit::{self, DailyEdit, LimitEdit};
use crate::storage::account::store::{load_limits, save_limits};
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::OperationContext;
use std::time::{Duration, Instant};

pub(crate) fn run(rclone: &str, command: LimitsCommands) -> Result<()> {
    match command {
        LimitsCommands::Show { json } => show(rclone, json),
        LimitsCommands::Set {
            remote,
            daily_upload_gib,
            no_daily_limit,
            default_daily_limit,
            bwlimit,
            tpslimit,
            max_uploads,
            max_downloads,
            inactivity_warn_days,
        } => {
            let daily = match (daily_upload_gib, no_daily_limit, default_daily_limit) {
                (Some(gib), _, _) => Some(DailyEdit::Gib(gib)),
                (None, true, _) => Some(DailyEdit::Unlimited),
                (None, false, true) => Some(DailyEdit::BackendDefault),
                _ => None,
            };
            let change = LimitEdit {
                daily,
                bwlimit,
                tpslimit,
                max_uploads,
                max_downloads,
                inactivity_warn_days,
            };
            if change == LimitEdit::default() {
                bail!("nothing to change; see `rpool provider limits set --help`");
            }
            let account = resolve_account(rclone, &remote);
            let mut store = load_limits()?;
            edit::apply(&mut store, &account, &change)?;
            let path = save_limits(&store)?;
            println!("Limits of {account} saved to {}", path.display());
            Ok(())
        }
        LimitsCommands::Reset { remote } => {
            let account = resolve_account(rclone, &remote);
            let mut store = load_limits()?;
            let removed = store.accounts.remove(&account).is_some();
            let path = save_limits(&store)?;
            if removed {
                println!(
                    "Limits of {account} reset to backend defaults ({})",
                    path.display()
                );
            } else {
                println!("{account} had no overrides; backend defaults apply");
            }
            Ok(())
        }
        LimitsCommands::Bandwidth { timetable } => {
            let mut store = load_limits()?;
            if let Some(text) = timetable {
                edit::set_bandwidth(&mut store, &text)?;
                save_limits(&store)?;
            }
            print_bandwidth(store.bandwidth.as_deref())
        }
        LimitsCommands::DefaultUploads { uploads } => {
            let mut store = load_limits()?;
            store.default_max_uploads = (uploads > 0).then_some(uploads);
            save_limits(&store)?;
            println!("{}", default_uploads_line(&store));
            Ok(())
        }
        LimitsCommands::DefaultDownloads { downloads } => {
            let mut store = load_limits()?;
            store.default_max_downloads = (downloads > 0).then_some(downloads);
            save_limits(&store)?;
            println!("{}", default_downloads_line(&store));
            Ok(())
        }
        LimitsCommands::KeepaliveDays { days } => {
            let mut store = load_limits()?;
            store.keepalive_days = days;
            save_limits(&store)?;
            if days == 0 {
                println!("Automatic keep-alive from mounts is off");
            } else {
                println!("Mounts keep their accounts alive after {days} idle days");
            }
            Ok(())
        }
    }
}

fn default_uploads_line(store: &crate::storage::account::limits::LimitsStore) -> String {
    match store.default_max_uploads {
        Some(n) => format!(
            "Simultaneous shard uploads per account: {n} by default (Dropbox 1; accounts may set their own)"
        ),
        None => "Simultaneous shard uploads per account: 16 by default (built in; Dropbox 1)".into(),
    }
}

fn default_downloads_line(store: &crate::storage::account::limits::LimitsStore) -> String {
    match store.default_max_downloads {
        Some(n) => format!(
            "Simultaneous shard downloads per account: {n} by default (accounts may set their own)"
        ),
        None => "Simultaneous shard downloads per account: 16 by default (built in)".into(),
    }
}

/// A crypt (or alias) remote names its account; anything else is the account.
fn resolve_account(rclone: &str, remote: &str) -> String {
    let name = remote.trim().trim_end_matches(':').to_owned();
    let context = RcloneContext::inherited(rclone);
    let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
    match context.config_dump(&ctx) {
        Ok(config) => crate::storage::rclone::write_account(&config, &name).0,
        Err(_) => name,
    }
}

fn print_bandwidth(text: Option<&str>) -> Result<()> {
    let Some(text) = text else {
        println!("Bandwidth: unlimited");
        return Ok(());
    };
    let table = crate::storage::account::bandwidth::Timetable::parse(text)?;
    let offset = crate::utils::local_offset_seconds();
    let now = crate::utils::now_unix();
    let (rate, next) = table.at_unix(now, offset.unwrap_or(0));
    println!("Bandwidth timetable: {text}");
    println!(
        "Now: {} ({})",
        rate.describe(),
        offset.map_or_else(
            || "local time unknown, using UTC".into(),
            crate::utils::offset_label
        )
    );
    if let Some(next) = next {
        println!("Next change in {}", wait_text(next.saturating_sub(now)));
    }
    Ok(())
}

fn show(rclone: &str, json: bool) -> Result<()> {
    let catalog = RcloneAdmin::inherited(rclone).catalog()?;
    let accounts = limits_view::accounts_from_catalog(&catalog);
    let store = load_limits()?;
    let ledger = match crate::storage::account::runtime::ledger() {
        Some(ledger) => ledger.load().unwrap_or_default(),
        None => Default::default(),
    };
    let now = crate::utils::now_unix();
    let rows = limits_view::build(&accounts, &ledger, &store, now);
    if json {
        let value = serde_json::json!({
            "bandwidth": store.bandwidth,
            "keepalive_days": store.keepalive_days,
            "default_max_uploads": store.default_max_uploads,
            "default_max_downloads": store.default_max_downloads,
            "accounts": rows,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!(
        "{:<22} {:<10} {:>26}  {:<26} LAST ACTIVITY",
        "ACCOUNT", "TYPE", "UPLOADED 24H / BUDGET", "STATE"
    );
    println!("{}", "-".repeat(104));
    for row in &rows {
        println!(
            "{:<22} {:<10} {:>26}  {:<26} {}",
            row.account,
            row.kind,
            budget_cell(row),
            state_cell(row, now),
            activity_cell(row)
        );
        let mut extra = Vec::new();
        if let Some(bw) = &row.bwlimit {
            extra.push(format!("bwlimit {bw}"));
        }
        if let Some(tps) = row.tpslimit {
            extra.push(format!("tpslimit {tps}"));
        }
        if let Some(n) = row.max_uploads {
            extra.push(format!("max uploads {n}"));
        }
        if let Some(n) = row.max_downloads {
            extra.push(format!("max downloads {n}"));
        }
        if !extra.is_empty() {
            println!("{:<22} {}", "", extra.join(" · "));
        }
    }
    println!();
    println!("{}", default_uploads_line(&store));
    println!("{}", default_downloads_line(&store));
    print_bandwidth(store.bandwidth.as_deref())?;
    println!(
        "Uploads are counted per computer; other computers and tools using the same account are not included. \
Inactivity is measured from this computer's last successful call; providers decide what counts as activity."
    );
    Ok(())
}

fn budget_cell(row: &AccountStatus) -> String {
    let used = format_bytes(row.uploaded_24h);
    match row.daily_upload_limit {
        Some(limit) => format!("{used} / {}", format_bytes(limit)),
        None => format!("{used} / unlimited"),
    }
}

fn state_cell(row: &AccountStatus, now: u64) -> String {
    match (row.pause_reason, row.paused_until_unix) {
        (Some("provider"), Some(until)) => {
            format!(
                "provider limit, retry {}",
                wait_text(until.saturating_sub(now))
            )
        }
        (Some(_), Some(until)) => {
            format!("paused, resumes {}", wait_text(until.saturating_sub(now)))
        }
        _ => "ok".into(),
    }
}

fn activity_cell(row: &AccountStatus) -> String {
    let age = match row.inactive_days {
        Some(0) => "today".to_string(),
        Some(days) => format!("{days} d ago"),
        None => "never (from this computer)".to_string(),
    };
    match (row.inactivity, row.inactivity_warn_days) {
        ("near", Some(warn)) => format!("{age} — near the {warn}-day warning"),
        ("exceeded", Some(warn)) => format!("{age} — over {warn} days: keep alive"),
        _ => age,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> AccountStatus {
        AccountStatus {
            account: "gd".into(),
            kind: "drive".into(),
            crypts: vec![],
            daily_upload_limit: Some(750_000_000_000),
            daily_upload_is_default: true,
            uploaded_24h: 1 << 30,
            next_release_unix: None,
            paused_until_unix: None,
            pause_reason: None,
            last_activity_unix: None,
            inactive_days: None,
            inactivity_warn_days: Some(548),
            inactivity_is_default: true,
            inactivity: "unknown",
            last_keepalive_unix: None,
            bwlimit: None,
            tpslimit: None,
            max_uploads: None,
            max_downloads: None,
        }
    }

    #[test]
    fn table_cells_describe_budget_pause_and_activity() {
        let mut r = row();
        assert_eq!(budget_cell(&r), "1.00 GiB / 698.49 GiB");
        assert_eq!(state_cell(&r, 0), "ok");
        assert_eq!(activity_cell(&r), "never (from this computer)");
        r.pause_reason = Some("budget");
        r.paused_until_unix = Some(3600);
        assert_eq!(state_cell(&r, 0), "paused, resumes 1 h 00 min");
        r.pause_reason = Some("provider");
        assert_eq!(state_cell(&r, 0), "provider limit, retry 1 h 00 min");
        r.inactive_days = Some(500);
        r.inactivity = "near";
        assert_eq!(activity_cell(&r), "500 d ago — near the 548-day warning");
        r.daily_upload_limit = None;
        assert!(budget_cell(&r).ends_with("unlimited"));
    }
}
