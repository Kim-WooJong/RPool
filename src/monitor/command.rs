//! `rpool mount monitor`: prints the live status of running mounts.
use super::model::{HistoryPoint, MountEntry, NetStatus};
use crate::cli::MonitorArgs;
use crate::presentation::format_bytes;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

#[derive(Serialize)]
struct Item<'a> {
    entry: &'a MountEntry,
    status: Option<NetStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history: Option<Vec<HistoryPoint>>,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| {
        p.canonicalize()
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    canonical(a) == canonical(b)
}

pub(crate) fn select(mounts: Vec<MountEntry>, args: &MonitorArgs) -> Vec<MountEntry> {
    mounts
        .into_iter()
        .filter(|m| args.pool.as_ref().is_none_or(|pool| &m.pool == pool))
        .filter(|m| {
            args.workspace
                .as_ref()
                .is_none_or(|w| same_dir(w, Path::new(&m.workspace)))
        })
        .collect()
}

/// Per-remote (upload, download) totals of `points`.
fn history_totals(points: &[HistoryPoint]) -> BTreeMap<&str, (u64, u64, u64, u64)> {
    let mut totals: BTreeMap<&str, (u64, u64, u64, u64)> = BTreeMap::new();
    for p in points {
        let t = totals.entry(&p.remote).or_default();
        t.0 += p.upload_bytes;
        t.1 += p.download_bytes;
        t.2 += p.ok_ops;
        t.3 += p.failed_ops;
    }
    totals
}

/// One selected mount: registry entry, live status, history.
pub(crate) type Row = (MountEntry, Option<NetStatus>, Option<Vec<HistoryPoint>>);

pub(crate) fn render_text(items: &[Row], minutes: Option<u64>, now: u64) -> String {
    if items.is_empty() {
        return "No mounted pools are running.\n".into();
    }
    let mut out = String::new();
    for (entry, status, history) in items {
        out.push_str(&super::format::mount(entry, status.as_ref(), now));
        if let (Some(points), Some(minutes)) = (history, minutes) {
            out.push_str(&format!("  last {minutes} min:"));
            let totals = history_totals(points);
            if totals.is_empty() {
                out.push_str(" no traffic");
            }
            for (remote, (up, down, ok, failed)) in totals {
                out.push_str(&format!(
                    "\n    {remote} up {} down {} ops {ok}/{failed}",
                    format_bytes(up),
                    format_bytes(down)
                ));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

pub(crate) fn run(args: &MonitorArgs) -> Result<()> {
    let terminal = std::io::stdout().is_terminal();
    loop {
        let now = crate::storage::rclone::traffic::now_unix();
        let mounts = select(super::active_mounts(), args);
        if mounts.is_empty() && !args.watch && (args.pool.is_some() || args.workspace.is_some()) {
            bail!("no running mount matches the selection");
        }
        let items: Vec<Row> = mounts
            .into_iter()
            .map(|entry| {
                let status = super::read_status(&entry);
                let history = args.history_minutes.map(|minutes| {
                    super::load_history(
                        Path::new(&entry.workspace),
                        now.saturating_sub(minutes * 60),
                    )
                });
                (entry, status, history)
            })
            .collect();
        let mut stdout = std::io::stdout().lock();
        if args.json {
            let json: Vec<Item<'_>> = items
                .iter()
                .map(|(entry, status, history)| Item {
                    entry,
                    status: status.clone(),
                    history: history.clone(),
                })
                .collect();
            if args.watch {
                writeln!(stdout, "{}", serde_json::to_string(&json)?)?;
            } else {
                writeln!(stdout, "{}", serde_json::to_string_pretty(&json)?)?;
            }
        } else {
            if args.watch && terminal {
                write!(stdout, "\x1b[2J\x1b[H")?;
            }
            write!(stdout, "{}", render_text(&items, args.history_minutes, now))?;
        }
        stdout.flush()?;
        drop(stdout);
        if !args.watch {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pool: &str, workspace: &str) -> MountEntry {
        MountEntry {
            id: "a".into(),
            pool: pool.into(),
            workspace: workspace.into(),
            mountpoint: "/m".into(),
            frontend: "fuse".into(),
            pid: 1,
            started_unix: 0,
        }
    }

    #[test]
    fn selection_by_pool_and_workspace() {
        let root = tempfile::tempdir().unwrap();
        let w = root.path().join("w");
        std::fs::create_dir(&w).unwrap();
        let mounts = vec![entry("a", &w.to_string_lossy()), entry("b", "/elsewhere")];
        let args = |pool: Option<&str>, workspace: Option<&Path>| MonitorArgs {
            workspace: workspace.map(Path::to_path_buf),
            pool: pool.map(str::to_owned),
            watch: false,
            json: false,
            history_minutes: None,
        };
        assert_eq!(select(mounts.clone(), &args(None, None)).len(), 2);
        assert_eq!(select(mounts.clone(), &args(Some("b"), None))[0].pool, "b");
        let by_dir = select(mounts.clone(), &args(None, Some(&w.join("."))));
        assert_eq!(by_dir.len(), 1);
        assert_eq!(by_dir[0].pool, "a");
        assert!(select(mounts, &args(Some("zzz"), None)).is_empty());
    }

    #[test]
    fn text_lists_history_totals_and_handles_no_mounts() {
        assert_eq!(render_text(&[], None, 0), "No mounted pools are running.\n");
        let point = |remote: &str, up| HistoryPoint {
            minute_unix: 60,
            remote: remote.into(),
            upload_bytes: up,
            download_bytes: 1,
            ok_ops: 1,
            failed_ops: 0,
        };
        let items = vec![(
            entry("p", "/w"),
            None,
            Some(vec![point("a:", 1024), point("a:", 1024), point("b:", 5)]),
        )];
        let text = render_text(&items, Some(15), 100);
        assert!(text.contains("last 15 min:"), "{text}");
        assert!(text.contains("a: up 2.00 KiB down 2 B ops 2/0"), "{text}");
        assert!(text.contains("b: up 5 B"), "{text}");
        let items = vec![(entry("p", "/w"), None, Some(vec![]))];
        assert!(render_text(&items, Some(5), 100).contains("no traffic"));
    }
}
