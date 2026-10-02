//! "Reclaimable space" in the "Trash & versions" card: what `rpool drive
//! cleanup` could delete (preview on "Check now", read as a query) and
//! "Clean up now" (`--confirm` in the task runner). Cleanup marks newly
//! found data and deletes only data whose grace period has ended; when the
//! mass-delete guard would stop it, a second confirmation adds `--force`.

use crate::drive_history::model::{CleanupMode, CleanupReport, CleanupTotals};
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::history::clock;
use crate::gui::screens::files::inventory::history::state::{Change, CleanupConfirm, HistoryForm};
use crate::gui::screens::files::inventory::history::{args, changes};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

/// Renders the "Reclaimable space" block for `pool`; called by
/// `retention_card` inside the Trash & versions card. Re-runs the preview
/// when it is stale and handles the confirm / force-confirm flow.
pub(crate) fn show(
    ui: &mut egui::Ui,
    history: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
    pool: &str,
) {
    let running = history
        .running
        .as_ref()
        .is_some_and(|(p, change)| p == pool && matches!(change, Change::Cleanup { .. }));
    let entry = history.pool(pool);
    if entry.cleanup.stale && entry.cleanup.value.is_some() && !entry.cleanup.is_loading() {
        entry.cleanup.start(rclone, args::cleanup_preview(pool));
    }
    if entry.cleanup.poll() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    ui.separator();
    ui.strong(tr("Reclaimable space"));
    let loading = entry.cleanup.is_loading();
    let report = match &entry.cleanup.value {
        Some(Ok(report)) => Some(report.clone()),
        Some(Err(error)) => {
            ui.colored_label(
                theme::error_colors(ui.visuals().dark_mode).1,
                trf("The check failed: {error}", &[("error", error)]),
            );
            None
        }
        None => {
            theme::hint(
                ui,
                tr("Deleted files and old versions keep using space on the accounts until a cleanup removes data nothing needs any more."),
            );
            None
        }
    };
    if let Some(report) = &report {
        summary(ui, report);
    }
    let reclaimable = report.as_ref().is_some_and(|r| {
        r.mode != CleanupMode::Postponed && r.candidates.archives + r.due.archives > 0
    });
    let idle = !running && !task.is_running();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!loading && !running, egui::Button::new(tr("Check now")))
            .clicked()
        {
            entry.cleanup.start(rclone, args::cleanup_preview(pool));
            entry.cleanup_confirm = CleanupConfirm::None;
        }
        if ui
            .add_enabled(
                reclaimable && idle && !loading && entry.cleanup_confirm == CleanupConfirm::None,
                egui::Button::new(tr("Clean up now")),
            )
            .clicked()
        {
            entry.cleanup_confirm = CleanupConfirm::First;
            entry.cleanup_notice = None;
        }
        if loading || running {
            ui.spinner();
        }
    });
    let mut start = None;
    if let Some(report) = &report {
        match entry.cleanup_confirm {
            CleanupConfirm::None => {}
            CleanupConfirm::First => match confirm(ui, report) {
                Some(true) if report.guard.is_some() => {
                    entry.cleanup_confirm = CleanupConfirm::Guard
                }
                Some(true) => start = Some(false),
                Some(false) => entry.cleanup_confirm = CleanupConfirm::None,
                None => {}
            },
            CleanupConfirm::Guard => match guard(ui, report) {
                Some(true) => start = Some(true),
                Some(false) => entry.cleanup_confirm = CleanupConfirm::None,
                None => {}
            },
        }
    }
    if let Some((success, text)) = &entry.cleanup_notice {
        let dark = ui.visuals().dark_mode;
        let color = if *success {
            theme::success_colors(dark).1
        } else {
            theme::error_colors(dark).1
        };
        ui.colored_label(color, text);
    }
    if let Some(force) = start {
        entry.cleanup_confirm = CleanupConfirm::None;
        changes::start(
            history,
            task,
            rclone,
            pool,
            Change::Cleanup { force },
            args::cleanup_run(pool, force),
        );
    }
}

/// Sums `(bytes, files)` over the given totals.
fn sum(parts: &[&CleanupTotals]) -> (u64, u64) {
    parts.iter().fold((0, 0), |(bytes, files), t| {
        (bytes + t.bytes, files + t.files)
    })
}

/// Report lines: reclaimable total, waiting and due amounts, per-account
/// sizes and the automatic-cleanup setting; or why cleanup is postponed.
fn summary(ui: &mut egui::Ui, report: &CleanupReport) {
    if report.mode == CleanupMode::Postponed {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            trf(
                "Cleanup is postponed until every reference can be read: {reason}",
                &[(
                    "reason",
                    &report.postponed.first().cloned().unwrap_or_default(),
                )],
            ),
        );
        return;
    }
    let (bytes, files) = sum(&[&report.candidates, &report.waiting, &report.due]);
    ui.label(trf(
        "Reclaimable: {size} ({n} files)",
        &[("size", &format_bytes(bytes)), ("n", &files)],
    ));
    if report.waiting.archives > 0 {
        let until = report
            .next_deletion_unix
            .map(|t| clock::format_local(t, clock::offset()))
            .unwrap_or_default();
        ui.label(trf(
            "Waiting: {size} until {date}",
            &[
                ("size", &format_bytes(report.waiting.bytes)),
                ("date", &until),
            ],
        ));
    }
    if report.due.archives > 0 {
        ui.label(trf(
            "Ready to delete now: {size}",
            &[("size", &format_bytes(report.due.bytes))],
        ));
    }
    for account in &report.accounts {
        ui.weak(trf(
            "{account}: {size}",
            &[
                ("account", &account.account),
                ("size", &format_bytes(account.bytes)),
            ],
        ));
    }
    let auto = if report.settings.auto {
        trf(
            "Automatic cleanup is on: data nothing needs is marked and deleted {n} days later.",
            &[("n", &report.settings.grace_days)],
        )
    } else {
        tr("Automatic cleanup is off for this pool.").into()
    };
    theme::hint(ui, &auto);
}

/// First confirmation: `Some(true)` confirmed, `Some(false)` cancelled.
fn confirm(ui: &mut egui::Ui, report: &CleanupReport) -> Option<bool> {
    let mut answer = None;
    ui.colored_label(
        ui.visuals().warn_fg_color,
        trf(
            "Newly found data ({new}) is marked now and deleted after the grace period. Data whose grace period has ended ({due}) is deleted permanently now and cannot be restored.",
            &[
                ("new", &format_bytes(report.candidates.bytes)),
                ("due", &format_bytes(report.due.bytes)),
            ],
        ),
    );
    ui.horizontal_wrapped(|ui| {
        if theme::danger_button(ui, true, tr("Clean up")).clicked() {
            answer = Some(true);
        }
        if ui.button(tr("Cancel")).clicked() {
            answer = Some(false);
        }
    });
    answer
}

/// Second confirmation when the mass-delete guard would stop the run.
fn guard(ui: &mut egui::Ui, report: &CleanupReport) -> Option<bool> {
    let mut answer = None;
    ui.colored_label(
        ui.visuals().warn_fg_color,
        trf(
            "The mass-delete guard stops this cleanup: {why}. Continue only if that much old data is expected.",
            &[("why", &report.guard.clone().unwrap_or_default())],
        ),
    );
    ui.horizontal_wrapped(|ui| {
        if theme::danger_button(ui, true, tr("I checked the numbers — continue anyway")).clicked()
        {
            answer = Some(true);
        }
        if ui.button(tr("Cancel")).clicked() {
            answer = Some(false);
        }
    });
    answer
}
