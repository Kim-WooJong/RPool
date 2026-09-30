//! Step 2: what the plan moves, how much and how long, and whether it fits.
use super::state::{self, short_id, Step};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::migration::model::Plan;
use crate::presentation::format_bytes;
use eframe::egui;

/// Label, tone of the quota verdict.
pub(super) fn quota_verdict(quota_ok: Option<bool>) -> (&'static str, StatusTone) {
    match quota_ok {
        Some(true) => (tr("Fits the free space"), StatusTone::Success),
        Some(false) => (tr("Does not fit the free space"), StatusTone::Error),
        None => (tr("Free space unknown"), StatusTone::Neutral),
    }
}

pub(super) fn summary(ui: &mut egui::Ui, plan: &Plan) {
    let c = &plan.counts;
    ui.horizontal_wrapped(|ui| {
        status_badge(
            ui,
            &trf("{n} unaffected", &[("n", &c.unaffected)]),
            StatusTone::Neutral,
        );
        status_badge(
            ui,
            &trf("{n} relocate", &[("n", &c.relocate)]),
            StatusTone::Info,
        );
        status_badge(
            ui,
            &trf("{n} re-encode", &[("n", &c.reencode)]),
            StatusTone::Info,
        );
        status_badge(
            ui,
            &trf("{n} lost", &[("n", &c.lost)]),
            if c.lost > 0 {
                StatusTone::Error
            } else {
                StatusTone::Neutral
            },
        );
        status_badge(
            ui,
            &trf("{n} unknown", &[("n", &c.unknown)]),
            if c.unknown > 0 {
                StatusTone::Warning
            } else {
                StatusTone::Neutral
            },
        );
    });
    egui::Grid::new("migration-review-grid")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label(tr("Download"));
            ui.label(format_bytes(plan.download_bytes));
            ui.end_row();
            ui.label(tr("Upload"));
            ui.label(format_bytes(plan.upload_bytes));
            ui.end_row();
            ui.label(tr("New cloud space"));
            ui.label(format_bytes(plan.new_storage_bytes))
                .on_hover_text(tr(
                    "Originals are kept until retired, so this is extra space.",
                ));
            ui.end_row();
            ui.label(tr("Estimated time"));
            ui.label(state::format_eta(plan.estimated_seconds));
            ui.end_row();
            ui.label(tr("Speeds used"));
            ui.label(match (plan.download_mib_s, plan.upload_mib_s) {
                (None, None) => tr("unknown").to_string(),
                (d, u) => format!(
                    "↓ {} / ↑ {} MiB/s",
                    d.map_or("?".into(), |v| format!("{v:.1}")),
                    u.map_or("?".into(), |v| format!("{v:.1}"))
                ),
            });
            ui.end_row();
            ui.label(tr("Quota"));
            let (label, tone) = quota_verdict(plan.quota_ok);
            status_badge(ui, label, tone);
            ui.end_row();
        });
    for note in &plan.notes {
        theme::hint(ui, &format!("• {note}"));
    }
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let Some(plan) = state.migration.plan.clone() else {
        state.migration.reset_to_plan();
        return;
    };
    ui.label(
        egui::RichText::new(trf(
            "Plan {id} for pool '{pool}'",
            &[("id", &short_id(&plan.migration_id)), ("pool", &plan.pool)],
        ))
        .strong(),
    );
    summary(ui, &plan);
    ui.colored_label(
        theme::warning_colors(ui.visuals().dark_mode).1,
        tr("Drive files are not included yet — use Apply pool changes below (Advanced / manual) for a mounted drive."),
    );
    if plan.counts.unknown > 0 {
        theme::hint(ui, tr("Unknown entries could not be checked (provider error). They are retried on the next run and never counted as lost."));
    }
    let idle = !task.is_running();
    let movable = plan.counts.relocate + plan.counts.reencode;
    ui.horizontal_wrapped(|ui| {
        let can_start = idle && movable > 0 && plan.quota_ok != Some(false);
        if theme::primary_button(ui, can_start, tr("Start migration")).clicked() {
            let id = plan.migration_id.clone();
            if let Err(error) = state.migration.start_run(task, &state.settings.rclone, &id) {
                state.migration.error = Some(error);
            }
        }
        if plan.counts.lost > 0
            && ui
                .button(trf("Lost files ({n})", &[("n", &plan.counts.lost)]))
                .clicked()
        {
            state.migration.step = Step::Lost;
        }
        if theme::danger_button(ui, idle, tr("Discard"))
            .on_hover_text(tr(
                "Marks this migration abandoned in the cloud. Nothing is deleted.",
            ))
            .clicked()
        {
            let id = plan.migration_id.clone();
            if let Err(error) = state
                .migration
                .start_abandon(task, &state.settings.rclone, &id)
            {
                state.migration.error = Some(error);
            }
        }
        if ui.button(tr("Back")).clicked() {
            state.migration.reset_to_plan();
        }
    });
    if movable == 0 {
        theme::hint(ui, tr("Nothing needs to move for this pool."));
    } else if plan.quota_ok == Some(false) {
        theme::hint(ui, tr("Free space on the new accounts is too small. Add an account or free space, then plan again."));
    }
}
