//! Arguments of `rpool drive`: trash, file versions, rollback, retention and
//! cleanup of a pool's online drive. Executed by `drive_history::command::run`.
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
/// Arguments of the `rpool drive` group.
pub(crate) struct DriveArgs {
    #[command(subcommand)]
    /// Selected `drive` subcommand.
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
/// Subcommands of `rpool drive`.
pub(crate) enum DriveCommands {
    /// Deleted files: list, restore, purge.
    Trash(TrashArgs),
    /// Previous versions of a file.
    Versions(VersionsArgs),
    /// Roll a folder or the whole drive back to a time (preview unless --confirm).
    Rollback {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
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
    /// Delete drive data no current file, trash entry or kept version needs
    /// any more (preview unless --confirm). Data is first marked and deleted
    /// by a later run once the grace period has passed.
    Cleanup {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
        target: DriveTarget,
        /// Mark new candidates and delete marked data past its grace period.
        #[arg(long, conflicts_with = "cancel")]
        confirm: bool,
        /// Delete even when it exceeds the mass-delete guard (half of the
        /// drive's stored data or 10,000 objects in one run).
        #[arg(long, requires = "confirm")]
        force: bool,
        /// Drop every pending mark (deletions already started go on).
        #[arg(long)]
        cancel: bool,
    },
    /// Backups a workspace switch left next to a workspace: list or remove.
    Backups(BackupsArgs),
}

#[derive(Args, Debug)]
/// Arguments of `rpool drive backups`.
pub(crate) struct BackupsArgs {
    #[command(subcommand)]
    /// Selected `backups` subcommand.
    pub(crate) command: BackupsCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool drive backups`.
pub(crate) enum BackupsCommands {
    /// List the backups of a workspace (size, kind, exported local writes).
    List {
        /// The drive workspace whose backups are listed.
        #[arg(long)]
        workspace: PathBuf,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete one backup folder (never the workspace itself).
    Remove {
        /// The drive workspace the backup belongs to.
        #[arg(long)]
        workspace: PathBuf,
        /// Backup folder to delete, as `list` shows it.
        #[arg(long)]
        backup: PathBuf,
        /// Also delete exported local-only writes (recovered-writes/), which
        /// may exist nowhere else.
        #[arg(long)]
        include_recovered: bool,
    },
}

#[derive(Args, Debug)]
/// Arguments of `rpool drive trash`.
pub(crate) struct TrashArgs {
    #[command(subcommand)]
    /// Selected `trash` subcommand.
    pub(crate) command: TrashCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool drive trash`.
pub(crate) enum TrashCommands {
    /// List the entries in the trash.
    List {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
        target: DriveTarget,
    },
    /// Restore entries to their original path (or `name (restored).ext`).
    Restore {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
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
        /// Pool, optional workspace and output format.
        target: DriveTarget,
        #[arg(
            long = "id",
            required_unless_present = "expired",
            conflicts_with = "expired"
        )]
        /// Trash entry id (`dir:/path` for a folder). Repeatable.
        ids: Vec<String>,
        /// Every entry whose trash period has ended.
        #[arg(long)]
        expired: bool,
        #[arg(long)]
        /// Apply the purge; without it only a preview is printed.
        confirm: bool,
    },
    /// Purge everything in the trash.
    Empty {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
        target: DriveTarget,
        #[arg(long, required = true)]
        /// Required confirmation; nothing is purged without it.
        confirm: bool,
    },
}

#[derive(Args, Debug)]
/// Arguments of `rpool drive versions`.
pub(crate) struct VersionsArgs {
    #[command(subcommand)]
    /// Selected `versions` subcommand.
    pub(crate) command: VersionsCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool drive versions`.
pub(crate) enum VersionsCommands {
    /// Versions of a file, newest first (including deletions).
    List {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
        target: DriveTarget,
        /// Drive path, e.g. `/Docs/report.docx`.
        #[arg(long)]
        path: String,
    },
    /// Make an old version current again (a new version) or restore it as a copy.
    Restore {
        #[command(flatten)]
        /// Pool, optional workspace and output format.
        target: DriveTarget,
        #[arg(long)]
        /// Drive path of the file, e.g. `/Docs/report.docx`.
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
/// Arguments of `rpool drive retention`.
pub(crate) struct RetentionArgs {
    #[command(subcommand)]
    /// Selected `retention` subcommand.
    pub(crate) command: RetentionCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool drive retention`.
pub(crate) enum RetentionCommands {
    /// Show the trash and version retention of a pool.
    Show {
        #[arg(long)]
        /// Storage pool name.
        pool: String,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Change retention (0 = unlimited). Saved with the pool settings.
    Set {
        #[arg(long)]
        /// Storage pool name.
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
        /// Let mounts delete unreferenced drive data automatically (daily).
        #[arg(long)]
        auto_cleanup: Option<bool>,
        /// Days marked data waits before it is deleted (at least 1).
        #[arg(long)]
        cleanup_grace_days: Option<u32>,
        #[arg(long)]
        /// Print JSON instead of text.
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

    #[test]
    fn cleanup_parses() {
        match drive(&["cleanup", "--pool", "p", "--confirm", "--force", "--json"]) {
            DriveCommands::Cleanup {
                target,
                confirm,
                force,
                cancel,
            } => assert!(target.json && confirm && force && !cancel),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            drive(&["cleanup", "--pool", "p", "--cancel"]),
            DriveCommands::Cleanup { cancel: true, .. }
        ));
        assert!(fails(&["cleanup", "--pool", "p", "--force"]));
        assert!(fails(&["cleanup", "--pool", "p", "--cancel", "--confirm"]));
        match drive(&[
            "retention",
            "set",
            "--pool",
            "p",
            "--auto-cleanup",
            "false",
            "--cleanup-grace-days",
            "14",
        ]) {
            DriveCommands::Retention(RetentionArgs {
                command:
                    RetentionCommands::Set {
                        auto_cleanup,
                        cleanup_grace_days,
                        ..
                    },
            }) => assert_eq!((auto_cleanup, cleanup_grace_days), (Some(false), Some(14))),
            other => panic!("{other:?}"),
        }
    }
}
