//! Core `rpool doctor` checks: config directory, rclone version and remotes,
//! remote-root overrides, pools, inventory, history and the scrub snapshot.
//! `run_checks` is used by `commands::doctor`, the GUI diagnostics screen and
//! the diagnostics bundle.
use crate::config::{
    app_config_dir, history_path, integrity_snapshot_path, inventory_path, pools_path,
    remote_roots_path,
};
use crate::history::load_history;
use crate::inventory::load_inventory;
use crate::maintenance::load_integrity_snapshot;
use crate::pool::{load_pool_store, validate_pool};
use crate::remote_root::load_remote_root_store;
use crate::storage::admin::{BackendAdmin, RcloneAdmin, ToolDiagnostics};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
/// One doctor result row; serialized into `rpool doctor --json` and `doctor.json`.
pub(crate) struct Diagnostic {
    /// Check identifier, e.g. `pools` or `rclone-version`.
    pub(crate) check: String,
    /// `ok`, `info`, `warn` or `fail`.
    pub(crate) status: String,
    /// Human-readable detail.
    pub(crate) message: String,
}

/// Run every check against the given rclone executable.
pub(crate) fn run_checks(rclone: &str) -> Vec<Diagnostic> {
    let adapter = RcloneAdmin::inherited(rclone);
    run_checks_with(Some(&adapter), Some(&adapter))
}

/// Run the checks with optional backends; `None` skips the rclone version
/// and/or remote discovery (used for `--local-only` bundles and tests).
pub(crate) fn run_checks_with(
    admin: Option<&dyn BackendAdmin>,
    tools: Option<&dyn ToolDiagnostics>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_config_dir(&mut out);
    if let Some(tools) = tools {
        check_rclone(tools, &mut out);
    } else {
        out.push(info(
            "rclone-version",
            "not required for local/native-only diagnostics",
        ));
    }
    let configured_remotes = if let Some(admin) = admin {
        match admin.discover() {
            Ok(remotes) => {
                out.push(ok(
                    "rclone-remotes",
                    format!("{} remotes discovered", remotes.len()),
                ));
                Some(remotes)
            }
            Err(error) => {
                out.push(fail("rclone-remotes", format!("{error:#}")));
                None
            }
        }
    } else {
        out.push(info(
            "rclone-remotes",
            "not checked: no legacy admin selected",
        ));
        None
    };
    check_remote_roots(configured_remotes.as_deref(), &mut out);
    check_pools(admin, configured_remotes.as_deref(), &mut out);
    check_inventory(&mut out);
    check_history(&mut out);
    check_integrity_snapshot(&mut out);
    out
}

/// Report the RPool config directory, or that it is created on first write.
fn check_config_dir(out: &mut Vec<Diagnostic>) {
    match app_config_dir() {
        Ok(path) if path.exists() => out.push(ok("config-directory", path.display().to_string())),
        Ok(path) => out.push(info(
            "config-directory",
            format!("{} (created on first write)", path.display()),
        )),
        Err(error) => out.push(fail("config-directory", format!("{error:#}"))),
    }
}

/// Report the rclone version plus the supported-version row (`rclone-support`).
fn check_rclone(tools: &dyn ToolDiagnostics, out: &mut Vec<Diagnostic>) {
    match tools.version() {
        Ok(version) => {
            let support = super::rclone_version::diagnostic(&version);
            out.push(ok("rclone-version", version));
            out.push(support);
        }
        Err(error) => out.push(fail("rclone-version", format!("{error:#}"))),
    }
}

/// Validate `remote_roots.json` and warn about overrides for unknown remotes.
fn check_remote_roots(configured_remotes: Option<&[String]>, out: &mut Vec<Diagnostic>) {
    match remote_roots_path() {
        Ok(path) if !path.exists() => {
            out.push(info(
                "remote-roots",
                "no per-remote default paths configured",
            ));
            return;
        }
        Err(error) => {
            out.push(fail("remote-roots", format!("{error:#}")));
            return;
        }
        _ => {}
    }

    match load_remote_root_store() {
        Ok(store) => {
            let mut unknown = Vec::new();
            if let Some(configured) = configured_remotes {
                for name in store.roots.keys() {
                    let target = format!("{name}:");
                    if !configured.iter().any(|remote| remote == &target) {
                        unknown.push(target);
                    }
                }
            }
            if unknown.is_empty() {
                out.push(ok(
                    "remote-roots",
                    format!("{} default path override(s) valid", store.roots.len()),
                ));
            } else {
                out.push(warn(
                    "remote-roots",
                    format!("unknown rclone remote(s): {}", unknown.join(", ")),
                ));
            }
        }
        Err(error) => out.push(fail("remote-roots", format!("{error:#}"))),
    }
}

/// Validate `pools.json`: invalid definitions or non-crypt targets fail;
/// remotes missing from rclone's config warn.
fn check_pools(
    admin: Option<&dyn BackendAdmin>,
    configured_remotes: Option<&[String]>,
    out: &mut Vec<Diagnostic>,
) {
    match pools_path() {
        Ok(path) if !path.exists() => {
            out.push(info("pools", "no pools.json yet"));
            return;
        }
        Err(error) => {
            out.push(fail("pools", format!("{error:#}")));
            return;
        }
        _ => {}
    }

    match load_pool_store() {
        Ok(store) => {
            let mut invalid = 0usize;
            let mut unencrypted = Vec::new();
            let mut missing = Vec::new();
            for (name, pool) in &store.pools {
                if validate_pool(pool).is_err() {
                    invalid += 1;
                }
                if let Some(admin) = admin {
                    for remote in &pool.remotes {
                        if let Err(error) = admin.ensure_encrypted(remote) {
                            unencrypted.push(format!("{name}: {error}"));
                        }
                    }
                }
                if let Some(configured) = configured_remotes {
                    for remote in &pool.remotes {
                        let root = remote_root(remote);
                        if !configured.iter().any(|candidate| candidate == &root) {
                            missing.push(format!("{name}:{root}"));
                        }
                    }
                }
            }
            if invalid > 0 {
                out.push(fail(
                    "pools",
                    format!("{} invalid pool definition(s)", invalid),
                ));
            } else if !unencrypted.is_empty() {
                out.push(fail(
                    "pools",
                    format!("non-crypt storage target(s): {}", unencrypted.join("; ")),
                ));
            } else if !missing.is_empty() {
                out.push(warn(
                    "pools",
                    format!("unknown rclone remote(s): {}", missing.join(", ")),
                ));
            } else if admin.is_none() {
                out.push(info(
                    "pools",
                    format!(
                        "{} local pool definition(s); encryption/remote checks not performed",
                        store.pools.len()
                    ),
                ));
            } else {
                out.push(ok("pools", format!("{} pool(s) valid", store.pools.len())));
            }
        }
        Err(error) => out.push(fail("pools", format!("{error:#}"))),
    }
}

/// Report whether the archive inventory loads and how many entries it has.
fn check_inventory(out: &mut Vec<Diagnostic>) {
    match inventory_path() {
        Ok(path) if !path.exists() => out.push(info("inventory", "inventory not created yet")),
        Err(error) => out.push(fail("inventory", format!("{error:#}"))),
        _ => match load_inventory() {
            Ok(store) => out.push(ok(
                "inventory",
                format!("{} archive(s) indexed", store.entries.len()),
            )),
            Err(error) => out.push(fail("inventory", format!("{error:#}"))),
        },
    }
}

/// Report whether the operation history loads.
fn check_history(out: &mut Vec<Diagnostic>) {
    match history_path() {
        Ok(path) if !path.exists() => out.push(info("history", "history not created yet")),
        Err(error) => out.push(fail("history", format!("{error:#}"))),
        _ => match load_history() {
            Ok(records) => out.push(ok(
                "history",
                format!("{} record(s) readable", records.len()),
            )),
            Err(error) => out.push(fail("history", format!("{error:#}"))),
        },
    }
}

/// Summarize the latest integrity (scrub) snapshot, if any.
fn check_integrity_snapshot(out: &mut Vec<Diagnostic>) {
    match integrity_snapshot_path() {
        Ok(path) if !path.exists() => {
            out.push(info("integrity-snapshot", "no scrub snapshot created yet"))
        }
        Err(error) => out.push(fail("integrity-snapshot", format!("{error:#}"))),
        _ => match load_integrity_snapshot() {
            Ok(Some(snapshot)) => out.push(ok(
                "integrity-snapshot",
                format!(
                    "archive={} checked={} total={} healthy={} errors={}",
                    snapshot.archive_id,
                    snapshot.checked_unix,
                    snapshot.total,
                    snapshot.healthy,
                    snapshot.errors
                ),
            )),
            Ok(None) => out.push(info("integrity-snapshot", "no scrub snapshot created yet")),
            Err(error) => out.push(fail("integrity-snapshot", format!("{error:#}"))),
        },
    }
}

/// `remote:path` -> `remote:` (the form rclone's remote listing uses).
fn remote_root(remote: &str) -> String {
    match remote.find(':') {
        Some(index) => remote[..=index].to_string(),
        None => remote.to_string(),
    }
}

/// An `ok` row.
fn ok(check: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        check: check.into(),
        status: "ok".to_string(),
        message: message.into(),
    }
}

/// An `info` row.
fn info(check: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        check: check.into(),
        status: "info".to_string(),
        message: message.into(),
    }
}

/// A `warn` row.
fn warn(check: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        check: check.into(),
        status: "warn".to_string(),
        message: message.into(),
    }
}

/// A `fail` row.
fn fail(check: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        check: check.into(),
        status: "fail".to_string(),
        message: message.into(),
    }
}
