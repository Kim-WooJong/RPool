//! `rpool mount monitor`. `rpool mount` takes only flags (its required
//! `--pool`/`--workspace` cannot coexist with a clap subcommand), so the
//! `monitor` word right after `mount` is recognised before the main parse.
use clap::Parser;
use std::ffi::OsString;
use std::path::PathBuf;

/// Live network traffic of mounted pools: per-account rates, byte totals,
/// operations, upload queue and alerts. Lists every running mount (also ones
/// started by the GUI) unless --workspace or --pool selects one.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(name = "rpool mount monitor")]
pub(crate) struct MonitorArgs {
    /// Only the mount of this workspace directory.
    #[arg(long, conflicts_with = "pool")]
    pub(crate) workspace: Option<PathBuf>,
    /// Only mounts of this pool.
    #[arg(long)]
    pub(crate) pool: Option<String>,
    /// Refresh every second until Ctrl-C.
    #[arg(long)]
    pub(crate) watch: bool,
    /// JSON array of {entry, status[, history]} (one line per refresh with --watch).
    #[arg(long)]
    pub(crate) json: bool,
    /// Also the per-minute history of the last N minutes.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=129_600))]
    pub(crate) history_minutes: Option<u64>,
}

/// The arguments after `mount monitor` when `argv` (with the program name)
/// is `rpool [--rclone X] mount monitor ...`.
pub(crate) fn monitor_argv(argv: &[OsString]) -> Option<Vec<OsString>> {
    let mut rest = argv.iter().skip(1);
    loop {
        let token = rest.next()?.to_str()?;
        if token == "--rclone" {
            rest.next()?;
        } else if token.starts_with("--rclone=") {
        } else if token == "mount" {
            return (rest.next()?.to_str()? == "monitor").then(|| rest.cloned().collect());
        } else {
            return None;
        }
    }
}

/// Parses `rpool ... mount monitor <args>`, `None` for every other command.
pub(crate) fn parse_monitor(argv: &[OsString]) -> Option<Result<MonitorArgs, clap::Error>> {
    let rest = monitor_argv(argv)?;
    Some(MonitorArgs::try_parse_from(
        std::iter::once(OsString::from("rpool mount monitor")).chain(rest),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn monitor_is_recognised_only_right_after_mount() {
        let parse = |args: &[&str]| parse_monitor(&argv(args)).map(|r| r.map_err(|e| e.kind()));
        assert_eq!(
            parse(&["rpool", "mount", "monitor"]),
            Some(Ok(MonitorArgs {
                workspace: None,
                pool: None,
                watch: false,
                json: false,
                history_minutes: None,
            }))
        );
        let parsed = parse(&[
            "rpool",
            "--rclone",
            "/x/rclone",
            "mount",
            "monitor",
            "--pool",
            "p",
            "--watch",
            "--json",
            "--history-minutes",
            "30",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.pool.as_deref(), Some("p"));
        assert!(parsed.watch && parsed.json);
        assert_eq!(parsed.history_minutes, Some(30));
        let parsed = parse(&[
            "rpool",
            "--rclone=r",
            "mount",
            "monitor",
            "--workspace",
            "/w",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.workspace, Some(PathBuf::from("/w")));
        assert!(matches!(
            parse(&[
                "rpool",
                "mount",
                "monitor",
                "--pool",
                "p",
                "--workspace",
                "/w"
            ]),
            Some(Err(clap::error::ErrorKind::ArgumentConflict))
        ));
        assert!(matches!(
            parse(&["rpool", "mount", "monitor", "--history-minutes", "0"]),
            Some(Err(clap::error::ErrorKind::ValueValidation))
        ));
        for other in [
            &["rpool"][..],
            &["rpool", "mount"],
            &["rpool", "mount", "--pool", "monitor"],
            &["rpool", "put", "mount", "monitor"],
            &["rpool", "--rclone"],
            &["rpool", "mount", "--monitor"],
        ] {
            assert!(parse(other).is_none(), "{other:?}");
        }
        // A normal mount still parses with the main parser.
        assert!(crate::cli::Cli::try_parse_from([
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/w",
            "--mountpoint=/m",
        ])
        .is_ok());
    }
}
