//! Command lines of `rpool drive trash|versions|rollback|retention`. Every
//! value is its own argv item after its flag; ids repeat `--id` so the CLI
//! may take one or many values per flag.

use crate::drive_history::model::Retention;
use std::ffi::OsString;

fn base(words: &[&str], pool: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["drive".into()];
    args.extend(words.iter().map(OsString::from));
    args.extend(["--pool".into(), pool.into()]);
    args
}

fn ids(args: &mut Vec<OsString>, ids: &[String]) {
    for id in ids {
        args.extend(["--id".into(), id.into()]);
    }
}

pub(crate) fn trash_list(pool: &str) -> Vec<OsString> {
    let mut args = base(&["trash", "list"], pool);
    args.push("--json".into());
    args
}

/// `to`: the destination folder (drive path) instead of the old place; every
/// entry keeps its name (`--into`).
pub(crate) fn trash_restore(pool: &str, entries: &[String], to: Option<&str>) -> Vec<OsString> {
    let mut args = base(&["trash", "restore"], pool);
    ids(&mut args, entries);
    if let Some(to) = to {
        args.extend(["--into".into(), to.into()]);
    }
    args
}

pub(crate) fn trash_purge(pool: &str, entries: &[String]) -> Vec<OsString> {
    let mut args = base(&["trash", "purge"], pool);
    ids(&mut args, entries);
    args.push("--confirm".into());
    args
}

pub(crate) fn trash_purge_expired(pool: &str) -> Vec<OsString> {
    let mut args = base(&["trash", "purge"], pool);
    args.extend(["--expired".into(), "--confirm".into()]);
    args
}

pub(crate) fn trash_empty(pool: &str) -> Vec<OsString> {
    let mut args = base(&["trash", "empty"], pool);
    args.push("--confirm".into());
    args
}

pub(crate) fn versions_list(pool: &str, path: &str) -> Vec<OsString> {
    let mut args = base(&["versions", "list"], pool);
    args.extend(["--path".into(), path.into(), "--json".into()]);
    args
}

pub(crate) fn versions_restore(pool: &str, path: &str, id: &str, as_copy: bool) -> Vec<OsString> {
    let mut args = base(&["versions", "restore"], pool);
    args.extend(["--path".into(), path.into(), "--id".into(), id.into()]);
    if as_copy {
        args.push("--as-copy".into());
    }
    args
}

/// Preview (`confirm == false`) or apply a rollback of `scope` (`/` is the
/// whole drive and passes no `--path`).
pub(crate) fn rollback(pool: &str, scope: &str, at: u64, confirm: bool) -> Vec<OsString> {
    let mut args = base(&["rollback"], pool);
    if scope != "/" && !scope.is_empty() {
        args.extend(["--path".into(), scope.into()]);
    }
    args.extend(["--at".into(), at.to_string().into(), "--json".into()]);
    if confirm {
        args.push("--confirm".into());
    }
    args
}

pub(crate) fn retention_show(pool: &str) -> Vec<OsString> {
    let mut args = base(&["retention", "show"], pool);
    args.push("--json".into());
    args
}

pub(crate) fn retention_set(pool: &str, retention: Retention) -> Vec<OsString> {
    let mut args = base(&["retention", "set"], pool);
    for (flag, value) in [
        ("--trash-days", retention.trash_days),
        ("--keep-versions", retention.keep_versions),
        ("--version-days", retention.version_days),
    ] {
        args.extend([flag.into(), value.to_string().into()]);
    }
    args
}

/// `drive cleanup` preview (read-only, captured as a query).
pub(crate) fn cleanup_preview(pool: &str) -> Vec<OsString> {
    let mut args = base(&["cleanup"], pool);
    args.push("--json".into());
    args
}

/// Confirmed `drive cleanup` (task runner); `force` passes the mass-delete guard.
pub(crate) fn cleanup_run(pool: &str, force: bool) -> Vec<OsString> {
    let mut args = base(&["cleanup"], pool);
    args.push("--confirm".into());
    if force {
        args.push("--force".into());
    }
    args.push("--json".into());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn trash_commands() {
        assert_eq!(
            s(trash_list("family")),
            ["drive", "trash", "list", "--pool", "family", "--json"]
        );
        let ids = vec!["e1".to_string(), "e 2".to_string()];
        assert_eq!(
            s(trash_restore("p", &ids, None)),
            ["drive", "trash", "restore", "--pool", "p", "--id", "e1", "--id", "e 2"]
        );
        assert_eq!(
            s(trash_restore("p", &ids[..1], Some("/My docs"))),
            ["drive", "trash", "restore", "--pool", "p", "--id", "e1", "--into", "/My docs"]
        );
        assert_eq!(
            s(trash_purge("p", &ids)),
            [
                "drive",
                "trash",
                "purge",
                "--pool",
                "p",
                "--id",
                "e1",
                "--id",
                "e 2",
                "--confirm"
            ]
        );
        assert_eq!(
            s(trash_purge_expired("p")),
            [
                "drive",
                "trash",
                "purge",
                "--pool",
                "p",
                "--expired",
                "--confirm"
            ]
        );
        assert_eq!(
            s(trash_empty("p")),
            ["drive", "trash", "empty", "--pool", "p", "--confirm"]
        );
    }

    #[test]
    fn version_commands() {
        assert_eq!(
            s(versions_list("p", "/Docs/-a.txt")),
            [
                "drive",
                "versions",
                "list",
                "--pool",
                "p",
                "--path",
                "/Docs/-a.txt",
                "--json"
            ]
        );
        assert_eq!(
            s(versions_restore("p", "/a", "r7", false)),
            ["drive", "versions", "restore", "--pool", "p", "--path", "/a", "--id", "r7"]
        );
        assert_eq!(
            s(versions_restore("p", "/a", "r7", true)).last().unwrap(),
            "--as-copy"
        );
    }

    #[test]
    fn rollback_and_retention_commands() {
        assert_eq!(
            s(rollback("p", "/", 100, false)),
            ["drive", "rollback", "--pool", "p", "--at", "100", "--json"]
        );
        assert_eq!(
            s(rollback("p", "/Docs", 100, true)),
            [
                "drive",
                "rollback",
                "--pool",
                "p",
                "--path",
                "/Docs",
                "--at",
                "100",
                "--json",
                "--confirm"
            ]
        );
        assert_eq!(
            s(retention_show("p")),
            ["drive", "retention", "show", "--pool", "p", "--json"]
        );
        let r = Retention {
            trash_days: 30,
            keep_versions: 0,
            version_days: 90,
        };
        assert_eq!(
            s(retention_set("p", r)),
            [
                "drive",
                "retention",
                "set",
                "--pool",
                "p",
                "--trash-days",
                "30",
                "--keep-versions",
                "0",
                "--version-days",
                "90"
            ]
        );
    }
}

/// Every command line the GUI builds is accepted by the real CLI.
#[cfg(test)]
mod cli_round_trip {
    use super::*;
    use clap::Parser;

    fn parses(args: Vec<OsString>) {
        let mut all = vec![OsString::from("rpool")];
        all.extend(args.iter().cloned());
        if let Err(error) = crate::cli::Cli::try_parse_from(&all) {
            panic!("{args:?} does not parse: {error}");
        }
    }

    #[test]
    fn gui_history_commands_parse_with_the_cli() {
        let ids = vec!["e1".to_string(), "dir:/Docs".to_string()];
        parses(trash_list("p"));
        parses(trash_restore("p", &ids, None));
        parses(trash_restore("p", &ids, Some("/My docs")));
        parses(trash_purge("p", &ids));
        parses(trash_purge_expired("p"));
        parses(trash_empty("p"));
        parses(versions_list("p", "/a b.txt"));
        parses(versions_restore("p", "/a.txt", "rev1", false));
        parses(versions_restore("p", "/a.txt", "rev1", true));
        parses(rollback("p", "/", 1_700_000_000, false));
        parses(rollback("p", "/Docs", 1_700_000_000, true));
        parses(retention_show("p"));
        parses(retention_set("p", Retention::default()));
        parses(cleanup_preview("p"));
        parses(cleanup_run("p", false));
        parses(cleanup_run("p", true));
        let s = |args: Vec<OsString>| -> Vec<String> {
            args.into_iter().map(|a| a.into_string().unwrap()).collect()
        };
        assert_eq!(
            s(cleanup_run("p", true)),
            [
                "drive",
                "cleanup",
                "--pool",
                "p",
                "--confirm",
                "--force",
                "--json"
            ]
        );
        assert_eq!(
            s(cleanup_preview("p")),
            ["drive", "cleanup", "--pool", "p", "--json"]
        );
    }
}
