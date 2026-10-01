use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct DriveArgs {
    #[command(subcommand)]
    pub(crate) command: DriveCommands,
}

/// Which drive, and how to print.
#[derive(Args, Debug, Clone)]
pub(crate) struct DriveTarget {
    /// Storage pool whose online drive is used.
    #[arg(long)]
    pub(crate) pool: String,
    /// Online-drive workspace of this PC. If it (or the pool) is mounted the
    /// request runs inside the mount; without one, listing reads the cloud.
    #[arg(long)]
    pub(crate) workspace: Option<PathBuf>,
    /// Print the JSON the GUI reads.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Subcommand, Debug)]
pub(crate) enum DriveCommands {
    /// Deleted files: list, restore, purge.
    Trash(TrashArgs),
    /// Previous versions of a file.
    Versions(VersionsArgs),
    /// Roll a folder or the whole drive back to a time (preview unless --confirm).
    Rollback {
        #[command(flatten)]
        target: DriveTarget,
        /// Folder to roll back (default: the whole drive).
        #[arg(long, default_value = "/")]
        path: String,
        /// Target time: RFC 3339, `YYYY-MM-DD[ HH:MM]` (local), Unix seconds or `2h ago`.
        #[arg(long)]
        at: String,
        /// Apply the changes (new revisions; files created later go to the trash).
        #[arg(long)]
        confirm: bool,
    },
    /// Trash and version retention of a pool.
    Retention(RetentionArgs),
}

#[derive(Args, Debug)]
pub(crate) struct TrashArgs {
    #[command(subcommand)]
    pub(crate) command: TrashCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum TrashCommands {
    List {
        #[command(flatten)]
        target: DriveTarget,
    },
    /// Restore entries to their original path (or `name (restored).ext`).
    Restore {
        #[command(flatten)]
        target: DriveTarget,
        /// Trash entry id (`dir:/path` for a folder). Repeatable.
        #[arg(long = "id", required = true)]
        ids: Vec<String>,
        /// Restore a single entry to this path instead.
        #[arg(long, conflicts_with = "into")]
        to: Option<String>,
        /// Restore every entry into this folder, keeping its name.
        #[arg(long)]
        into: Option<String>,
    },
    /// Remove entries from the trash on every PC (preview unless --confirm).
    Purge {
        #[command(flatten)]
        target: DriveTarget,
        #[arg(
            long = "id",
            required_unless_present = "expired",
            conflicts_with = "expired"
        )]
        ids: Vec<String>,
        /// Every entry whose trash period has ended.
        #[arg(long)]
        expired: bool,
        #[arg(long)]
        confirm: bool,
    },
    /// Purge everything in the trash.
    Empty {
        #[command(flatten)]
        target: DriveTarget,
        #[arg(long, required = true)]
        confirm: bool,
    },
}

#[derive(Args, Debug)]
pub(crate) struct VersionsArgs {
    #[command(subcommand)]
    pub(crate) command: VersionsCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum VersionsCommands {
    /// Versions of a file, newest first (including deletions).
    List {
        #[command(flatten)]
        target: DriveTarget,
        /// Drive path, e.g. `/Docs/report.docx`.
        #[arg(long)]
        path: String,
    },
    /// Make an old version current again (a new version) or restore it as a copy.
    Restore {
        #[command(flatten)]
        target: DriveTarget,
        #[arg(long)]
        path: String,
        /// Version id from `versions list`.
        #[arg(long)]
        id: String,
        /// Write it beside the file as `name (restored).ext`.
        #[arg(long)]
        as_copy: bool,
    },
}

#[derive(Args, Debug)]
pub(crate) struct RetentionArgs {
    #[command(subcommand)]
    pub(crate) command: RetentionCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum RetentionCommands {
    Show {
        #[arg(long)]
        pool: String,
        #[arg(long)]
        json: bool,
    },
    /// Change retention (0 = unlimited). Saved with the pool settings.
    Set {
        #[arg(long)]
        pool: String,
        /// Days a deleted file stays in the trash.
        #[arg(long)]
        trash_days: Option<u32>,
        /// Previous versions kept per file.
        #[arg(long)]
        keep_versions: Option<u32>,
        /// Days previous versions are kept.
        #[arg(long)]
        version_days: Option<u32>,
        #[arg(long)]
        json: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    fn drive(args: &[&str]) -> DriveCommands {
        let argv: Vec<&str> = ["rpool", "drive"].iter().chain(args).copied().collect();
        match Cli::try_parse_from(argv).unwrap().command {
            Some(Commands::Drive(DriveArgs { command })) => command,
            other => panic!("{other:?}"),
        }
    }
    fn fails(args: &[&str]) -> bool {
        let argv: Vec<&str> = ["rpool", "drive"].iter().chain(args).copied().collect();
        Cli::try_parse_from(argv).is_err()
    }

    #[test]
    fn trash_commands_parse() {
        match drive(&["trash", "list", "--pool", "p", "--json"]) {
            DriveCommands::Trash(TrashArgs {
                command: TrashCommands::List { target },
            }) => assert!(target.json && target.pool == "p" && target.workspace.is_none()),
            other => panic!("{other:?}"),
        }
        match drive(&[
            "trash",
            "restore",
            "--pool",
            "p",
            "--id",
            "a",
            "--id",
            "dir:/D",
            "--to",
            "/x",
            "--workspace",
            "/w",
        ]) {
            DriveCommands::Trash(TrashArgs {
                command:
                    TrashCommands::Restore {
                        target, ids, to, ..
                    },
            }) => {
                assert_eq!(ids, ["a", "dir:/D"]);
                assert_eq!(to.as_deref(), Some("/x"));
                assert_eq!(target.workspace, Some(PathBuf::from("/w")));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            drive(&["trash", "purge", "--pool", "p", "--expired", "--confirm"]),
            DriveCommands::Trash(TrashArgs {
                command: TrashCommands::Purge {
                    expired: true,
                    confirm: true,
                    ..
                }
            })
        ));
        assert!(matches!(
            drive(&["trash", "purge", "--pool", "p", "--id", "x"]),
            DriveCommands::Trash(TrashArgs {
                command: TrashCommands::Purge {
                    expired: false,
                    confirm: false,
                    ..
                }
            })
        ));
        assert!(fails(&["trash", "purge", "--pool", "p"]));
        assert!(fails(&[
            "trash",
            "purge",
            "--pool",
            "p",
            "--id",
            "x",
            "--expired"
        ]));
        assert!(fails(&["trash", "restore", "--pool", "p"]));
        assert!(fails(&["trash", "empty", "--pool", "p"]));
        assert!(matches!(
            drive(&["trash", "empty", "--pool", "p", "--confirm"]),
            DriveCommands::Trash(TrashArgs {
                command: TrashCommands::Empty { confirm: true, .. }
            })
        ));
        assert!(fails(&["trash", "list"]));
    }

    #[test]
    fn versions_rollback_and_retention_parse() {
        match drive(&["versions", "list", "--pool", "p", "--path", "/a.txt"]) {
            DriveCommands::Versions(VersionsArgs {
                command: VersionsCommands::List { path, .. },
            }) => assert_eq!(path, "/a.txt"),
            other => panic!("{other:?}"),
        }
        match drive(&[
            "versions",
            "restore",
            "--pool",
            "p",
            "--path",
            "/a",
            "--id",
            "r",
            "--as-copy",
        ]) {
            DriveCommands::Versions(VersionsArgs {
                command: VersionsCommands::Restore { id, as_copy, .. },
            }) => assert!(id == "r" && as_copy),
            other => panic!("{other:?}"),
        }
        assert!(fails(&[
            "versions", "restore", "--pool", "p", "--path", "/a"
        ]));
        match drive(&["rollback", "--pool", "p", "--at", "2h ago"]) {
            DriveCommands::Rollback {
                path, at, confirm, ..
            } => {
                assert!(path == "/" && at == "2h ago" && !confirm)
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            drive(&[
                "rollback",
                "--pool",
                "p",
                "--path",
                "/Docs",
                "--at",
                "1790726400",
                "--confirm",
                "--json"
            ]),
            DriveCommands::Rollback {
                confirm: true,
                target: DriveTarget { json: true, .. },
                ..
            }
        ));
        assert!(fails(&["rollback", "--pool", "p"]));
        assert!(matches!(
            drive(&["retention", "show", "--pool", "p"]),
            DriveCommands::Retention(RetentionArgs {
                command: RetentionCommands::Show { .. }
            })
        ));
        match drive(&[
            "retention",
            "set",
            "--pool",
            "p",
            "--trash-days",
            "7",
            "--keep-versions",
            "0",
        ]) {
            DriveCommands::Retention(RetentionArgs {
                command:
                    RetentionCommands::Set {
                        trash_days,
                        keep_versions,
                        version_days,
                        ..
                    },
            }) => assert_eq!(
                (trash_days, keep_versions, version_days),
                (Some(7), Some(0), None)
            ),
            other => panic!("{other:?}"),
        }
    }
}
