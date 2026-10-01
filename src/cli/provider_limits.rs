//! `rpool provider limits …`: daily upload budgets, bandwidth, request rate
//! and inactivity warnings per account, and the global bandwidth timetable.
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub(crate) struct LimitsArgs {
    #[command(subcommand)]
    pub(crate) command: LimitsCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum LimitsCommands {
    /// Show each account's upload budget, pause, last activity and limits.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Override limits of one account (the backing remote, e.g. `gdrive_1`).
    Set {
        /// Account remote name (a crypt remote is resolved to its account).
        #[arg(long)]
        remote: String,
        /// Daily (rolling 24 h) upload budget in GiB.
        #[arg(long, conflicts_with_all = ["no_daily_limit", "default_daily_limit"])]
        daily_upload_gib: Option<f64>,
        /// No daily upload budget for this account.
        #[arg(long, conflicts_with = "default_daily_limit")]
        no_daily_limit: bool,
        /// Back to the backend default budget (Google Drive: 750 GB).
        #[arg(long)]
        default_daily_limit: bool,
        /// Bandwidth for this account in `--bwlimit` syntax (`off` clears).
        #[arg(long)]
        bwlimit: Option<String>,
        /// rclone `--tpslimit` for this account (`0` clears).
        #[arg(long)]
        tpslimit: Option<f64>,
        /// Warn after this many days without activity (`0` = never warn).
        #[arg(long)]
        inactivity_warn_days: Option<u32>,
    },
    /// Remove every override of one account (backend defaults apply).
    Reset {
        #[arg(long)]
        remote: String,
    },
    /// Show or set the global bandwidth timetable, e.g.
    /// "08:00,512k 18:00,30M 23:00,off" or "10M:2M" (upload:download).
    Bandwidth {
        /// New timetable; `off` removes the limit. Omit to show the current one.
        timetable: Option<String>,
    },
    /// Days without activity after which a mount keeps its accounts alive
    /// automatically (`0` = never).
    KeepaliveDays { days: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands, ProviderCommands};
    use clap::Parser;

    fn limits(argv: &[&str]) -> Result<LimitsCommands, clap::Error> {
        let argv = ["rpool", "provider", "limits"].iter().chain(argv);
        match Cli::try_parse_from(argv)?.command {
            Some(Commands::Provider(args)) => match args.command {
                ProviderCommands::Limits(limits) => Ok(limits.command),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn limits_commands_parse() {
        assert!(matches!(
            limits(&["show", "--json"]).unwrap(),
            LimitsCommands::Show { json: true }
        ));
        match limits(&[
            "set",
            "--remote",
            "gd",
            "--daily-upload-gib",
            "700",
            "--bwlimit",
            "08:00,1M 18:00,off",
            "--tpslimit",
            "4",
            "--inactivity-warn-days",
            "365",
        ])
        .unwrap()
        {
            LimitsCommands::Set {
                remote,
                daily_upload_gib,
                bwlimit,
                tpslimit,
                inactivity_warn_days,
                ..
            } => {
                assert_eq!(remote, "gd");
                assert_eq!(daily_upload_gib, Some(700.0));
                assert_eq!(bwlimit.as_deref(), Some("08:00,1M 18:00,off"));
                assert_eq!((tpslimit, inactivity_warn_days), (Some(4.0), Some(365)));
            }
            other => panic!("{other:?}"),
        }
        assert!(limits(&["set", "--remote", "gd", "--no-daily-limit"]).is_ok());
        assert!(limits(&[
            "set",
            "--remote",
            "gd",
            "--daily-upload-gib",
            "1",
            "--no-daily-limit"
        ])
        .is_err());
        assert!(limits(&[
            "set",
            "--remote",
            "gd",
            "--no-daily-limit",
            "--default-daily-limit"
        ])
        .is_err());
        assert!(limits(&["set", "--daily-upload-gib", "1"]).is_err());
        assert!(matches!(
            limits(&["bandwidth", "08:00,512k 23:00,off"]).unwrap(),
            LimitsCommands::Bandwidth { timetable: Some(t) } if t == "08:00,512k 23:00,off"
        ));
        assert!(matches!(
            limits(&["bandwidth"]).unwrap(),
            LimitsCommands::Bandwidth { timetable: None }
        ));
        assert!(matches!(
            limits(&["keepalive-days", "0"]).unwrap(),
            LimitsCommands::KeepaliveDays { days: 0 }
        ));
        assert!(matches!(
            limits(&["reset", "--remote", "gd"]).unwrap(),
            LimitsCommands::Reset { .. }
        ));
    }

    #[test]
    fn keepalive_parses() {
        let cli = Cli::try_parse_from([
            "rpool",
            "provider",
            "keepalive",
            "--remote",
            "gd",
            "--remote",
            "od",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Provider(args)) => match args.command {
                ProviderCommands::Keepalive { remotes, json } => {
                    assert_eq!(remotes, ["gd", "od"]);
                    assert!(json);
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }
}
