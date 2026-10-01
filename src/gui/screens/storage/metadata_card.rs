//! "Drive metadata" card on the Pools page: record/checkpoint counts and
//! "Compact now", by running `rpool pool compact <pool> [--dry-run] --json`.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::{JobStatus, LogKind, LogLine, TaskRunner};
use crate::gui::theme;
use crate::mount::metadata_compaction::Report;
use eframe::egui;
use std::collections::BTreeMap;
use std::ffi::OsString;

/// Task name; identifies a finished metadata task (not shown translated).
const TASK: &str = "Drive metadata";

/// Last result per pool for this session (never saved).
#[derive(Debug, Default)]
pub(crate) struct MetadataForm {
    pub(crate) running: Option<String>,
    pub(crate) results: BTreeMap<String, Option<Report>>,
    pub(crate) notices: BTreeMap<String, String>,
}

pub(crate) fn pool_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let saved = [state.pools.name.trim(), state.pools.selected.as_str()]
        .into_iter()
        .find(|name| state.pool_definitions.contains_key(*name))
        .map(str::to_string);
    theme::card_section(
        ui,
        tr("Drive metadata"),
        Some(tr("Checkpoints let a new PC open the drive from a few objects instead of reading every file record.")),
        |_| {},
        |ui| {
            let Some(pool) = saved else {
                theme::hint(ui, tr("Select and load a saved pool to check its metadata."));
                return;
            };
            body(ui, &mut state.metadata, task, &state.settings.rclone, &pool);
        },
    );
}

fn body(
    ui: &mut egui::Ui,
    form: &mut MetadataForm,
    task: &mut TaskRunner,
    rclone: &str,
    pool: &str,
) {
    let idle = !task.is_running();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(idle, egui::Button::new(tr("Check metadata")))
            .clicked()
        {
            start(form, task, rclone, pool, true);
        }
        if theme::primary_button(ui, idle, tr("Compact now")).clicked() {
            start(form, task, rclone, pool, false);
        }
    });
    if task.is_running() && form.running.as_deref() == Some(pool) {
        theme::hint(ui, tr("Reading the pool's metadata…"));
    }
    if let Some(notice) = form.notices.get(pool) {
        ui.label(notice);
    }
    match form.results.get(pool) {
        Some(Some(report)) => {
            for line in summary(report) {
                ui.label(line);
            }
        }
        Some(None) => {
            ui.label(tr("This pool has no drive metadata yet."));
        }
        None => {}
    }
    theme::hint(
        ui,
        tr("Deleting checkpointed records is a one-time opt-in from the command line (`rpool pool compact <pool> --enable-deletion`), only after every PC is upgraded."),
    );
}

/// Translated lines for one report.
pub(crate) fn summary(r: &Report) -> Vec<String> {
    let mut lines = vec![trf(
        "Metadata: {records} records, {checkpoints} checkpoints, {uncovered} not yet in a checkpoint",
        &[
            ("records", &r.records),
            ("checkpoints", &r.checkpoints),
            ("uncovered", &r.uncovered),
        ],
    )];
    if r.checkpointed_records > 0 {
        lines.push(if r.dry_run {
            trf(
                "Compact now would checkpoint {n} records.",
                &[("n", &r.checkpointed_records)],
            )
        } else {
            trf(
                "Checkpointed {n} records.",
                &[("n", &r.checkpointed_records)],
            )
        });
    }
    lines.push(if r.deletion_enabled {
        trf(
            "Deletion of checkpointed records is on: {deleted} deleted, {deletable} due.",
            &[("deleted", &r.deleted), ("deletable", &r.deletable)],
        )
    } else {
        tr("Deletion of checkpointed records is off.").to_string()
    });
    if r.broken_checkpoints > 0 {
        lines.push(trf(
            "{n} unreadable checkpoints were ignored.",
            &[("n", &r.broken_checkpoints)],
        ));
    }
    lines
}

pub(crate) fn args(pool: &str, dry_run: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["pool".into(), "compact".into(), "--json".into()];
    if dry_run {
        args.push("--dry-run".into());
    }
    args.extend(["--".into(), pool.into()]);
    args
}

fn start(form: &mut MetadataForm, task: &mut TaskRunner, rclone: &str, pool: &str, dry_run: bool) {
    match task.start_rpool(TASK, rclone, args(pool, dry_run)) {
        Ok(()) => {
            form.running = Some(pool.to_string());
            form.notices.remove(pool);
        }
        Err(error) => {
            form.notices.insert(pool.to_string(), error);
        }
    }
}

/// The JSON report (`null` when the pool has no metadata) from stdout.
pub(crate) fn parse_report(logs: &[LogLine]) -> Option<Option<Report>> {
    let stdout: Vec<&str> = logs
        .iter()
        .filter(|line| line.kind == LogKind::Stdout)
        .map(|line| line.text.as_str())
        .collect();
    (0..stdout.len()).rev().find_map(|start| {
        let first = stdout[start].trim_start();
        if !(first.starts_with('{') || first == "null") {
            return None;
        }
        serde_json::from_str(&stdout[start..].join("\n")).ok()
    })
}

/// After any task: store the report of a finished metadata task.
pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    if task.last_task().is_none_or(|last| last.name != TASK) {
        return;
    }
    let Some(pool) = state.metadata.running.take() else {
        return;
    };
    match (parse_report(task.logs()), status) {
        (Some(report), JobStatus::Completed) => {
            state.metadata.results.insert(pool.clone(), report);
            state.metadata.notices.remove(&pool);
        }
        _ => {
            state.metadata.notices.insert(
                pool,
                tr("Metadata task failed; see Jobs for details.").to_string(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stdout(text: &str) -> Vec<LogLine> {
        text.lines()
            .map(|line| LogLine {
                kind: LogKind::Stdout,
                text: line.into(),
            })
            .collect()
    }
    #[test]
    fn args_and_report_parsing() {
        assert_eq!(
            args("p", true),
            ["pool", "compact", "--json", "--dry-run", "--", "p"]
                .map(OsString::from)
                .to_vec()
        );
        let report = Report {
            family: "v6".into(),
            records: 5,
            checkpointed_records: 5,
            ..Default::default()
        };
        let text = format!(
            "progress\n{}",
            serde_json::to_string_pretty(&Some(&report)).unwrap()
        );
        assert_eq!(parse_report(&stdout(&text)), Some(Some(report.clone())));
        assert_eq!(parse_report(&stdout("null")), Some(None));
        assert_eq!(parse_report(&stdout("no json")), None);
        let lines = summary(&report);
        assert!(lines[0].contains("5 records"));
        assert!(lines.iter().any(|l| l.contains("Checkpointed 5")));
    }
}
