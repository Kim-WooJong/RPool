//! Storage › Account changes › Pool change migration: plan what moves when
//! a pool's accounts or coding change, review it, run it (pause / resume
//! from any PC) and list the files that cannot be recovered. Mirrors
//! `rpool pool migrate plan|run|status|lost|abandon`.
mod lost;
mod plan_step;
mod review_step;
mod run_step;
pub(crate) mod state;
#[cfg(test)]
pub(crate) mod tests;

pub(crate) use state::MigrationForm;

use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;
use state::Step;

pub(crate) fn card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let form = &mut state.migration;
    if form.pool.is_empty() {
        if let Some(first) = state.pool_names.first() {
            form.select_pool(&first.clone());
        }
    }
    form.watch_task(task);
    let busy = form.poll();
    if form.status_due(std::time::Instant::now()) {
        form.start_status(&state.settings.rclone);
    }
    if busy || form.step == Step::Run {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    theme::card_section(
        ui,
        tr("Pool change migration"),
        Some(tr("After accounts leave or join a pool, or K / M / shard size / native crypt change: see what moves and how long it takes, then move it. Progress is kept in the cloud so any PC can resume. Nothing is deleted.")),
        |_| {},
        |ui| {
            steps_bar(ui, state.migration.step);
            if let Some(error) = &state.migration.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(notice) = &state.migration.notice {
                ui.label(notice);
            }
            match state.migration.step {
                Step::Plan => plan_step::show(ui, state, task),
                Step::Review => review_step::show(ui, state, task),
                Step::Run => run_step::show(ui, state, task),
                Step::Lost => lost::show(ui, state),
            }
            if matches!(state.migration.step, Step::Plan) {
                ui.separator();
                run_step::existing(ui, state, task);
            }
        },
    );
}

fn steps_bar(ui: &mut egui::Ui, step: Step) {
    let p = theme::pal(ui);
    ui.horizontal_wrapped(|ui| {
        for (index, (value, label)) in [
            (Step::Plan, tr("Plan")),
            (Step::Review, tr("Review")),
            (Step::Run, tr("Run")),
            (Step::Lost, tr("Lost files")),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                ui.label(egui::RichText::new("›").color(p.muted));
            }
            let text = egui::RichText::new(format!("{}. {label}", index + 1));
            ui.label(if value == step {
                text.strong().color(p.accent)
            } else {
                text.color(p.muted)
            });
        }
    });
}
