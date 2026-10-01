//! Step 3: run / pause / resume a migration, and the list of migrations of
//! the selected pool recorded in the cloud.
use super::state::{short_id, Step, Watched};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::migration::model::MigrationStatus;
use eframe::egui;

pub(super) fn state_label(status: &MigrationStatus) -> (&'static str, StatusTone) {
    if status.abandoned {
        (tr("Discarded"), StatusTone::Neutral)
    } else if status.complete {
        (tr("Complete"), StatusTone::Success)
    } else if status.switched > 0 || status.verified > 0 {
        (tr("In progress"), StatusTone::Info)
    } else {
        (tr("Not started"), StatusTone::Warning)
    }
}

pub(super) fn progress_text(status: &MigrationStatus) -> String {
    trf(
        "{done}/{total} switched",
        &[("done", &status.switched), ("total", &status.to_move)],
    )
}

fn our_run_active(state: &GuiState, task: &TaskRunner, id: &str) -> bool {
    task.is_running() && state.migration.watched == Some(Watched::Run(id.to_string()))
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let Some(id) = state.migration.active_id.clone() else {
        state.migration.reset_to_plan();
        return;
    };
    let running = our_run_active(state, task, &id);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(trf("Migration {id}", &[("id", &short_id(&id))])).strong());
        if running {
            status_badge(
                ui,
                if state.migration.pausing {
                    tr("Pausing after the archives in progress finish…")
                } else {
                    tr("Running")
                },
                StatusTone::Info,
            );
        } else if let Some(status) = state.migration.active_status() {
            let (label, tone) = state_label(status);
            status_badge(ui, label, tone);
        }
        if state.migration.status_loading() {
            ui.spinner();
        }
    });

    let status = state.migration.active_status().cloned();
    match &status {
        Some(status) => {
            let fraction = if status.to_move == 0 {
                1.0
            } else {
                status.switched as f32 / status.to_move as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_height(theme::PROGRESS_BAR_HEIGHT)
                    .text(progress_text(status)),
            );
            ui.horizontal_wrapped(|ui| {
                status_badge(
                    ui,
                    &trf("{n} verified", &[("n", &status.verified)]),
                    StatusTone::Neutral,
                );
                status_badge(
                    ui,
                    &trf("{n} lost", &[("n", &status.lost.len())]),
                    if status.lost.is_empty() {
                        StatusTone::Neutral
                    } else {
                        StatusTone::Error
                    },
                );
                if status.failed_unknown > 0 {
                    status_badge(
                        ui,
                        &trf("{n} unknown (retry)", &[("n", &status.failed_unknown)]),
                        StatusTone::Warning,
                    );
                }
            });
            if !status.pcs.is_empty() {
                theme::hint(
                    ui,
                    &trf("Worked on by: {pcs}", &[("pcs", &status.pcs.join(", "))]),
                );
            }
        }
        None => theme::hint(
            ui,
            state
                .migration
                .status_error
                .as_deref()
                .unwrap_or(tr("Loading progress from the cloud journal…")),
        ),
    }
    let drive = state.migration.active_drive_status().cloned();
    if let Some(drive) = &drive {
        super::drive_part::progress(ui, drive);
    }
    if running {
        if let Some(current) = task.current_task() {
            if let Some(fraction) = current.progress.fraction() {
                ui.add(egui::ProgressBar::new(fraction).text(tr("current entry")));
            }
        }
        if let Some(line) = task.logs().last() {
            theme::hint(ui, &line.text);
        }
    }

    let idle = !task.is_running();
    let finished = status.as_ref().is_some_and(|s| s.complete || s.abandoned);
    ui.horizontal_wrapped(|ui| {
        if running {
            if ui
                .add_enabled(!state.migration.pausing, egui::Button::new(tr("Pause")))
                .on_hover_text(tr("Stops cleanly after the current entry. Resume later from any PC."))
                .clicked()
            {
                if let Err(error) = state.migration.request_pause() {
                    state.migration.error = Some(error);
                }
            }
        } else {
            if theme::primary_button(ui, idle && !finished, tr("Resume")).clicked() {
                if let Err(error) = state.migration.start_run(task, &state.settings.rclone, &id) {
                    state.migration.error = Some(error);
                }
            }
            ui.checkbox(&mut state.migration.take_over, tr("Take over work of a stopped PC"))
                .on_hover_text(tr("Use only if the other PC crashed or was turned off. Its unfinished claims otherwise expire after 2 hours."));
            let hint = tr("How many archives migrate at the same time (1 = one after another). Each one also uses the pool's workers, so higher values mean more simultaneous transfers.");
            ui.label(tr("Archives at once")).on_hover_text(hint);
            ui.add(
                egui::DragValue::new(&mut state.migration.parallel.0)
                    .range(1..=crate::migration::execute::MAX_PARALLEL),
            )
            .on_hover_text(hint);
        }
        if let Some(drive) = &drive {
            let label = if drive.adopted.is_some() {
                tr("Drive adopted…")
            } else {
                tr("Adopt drive…")
            };
            if ui
                .button(label)
                .on_hover_text(tr("Switch the drive to the migrated layout on every PC."))
                .clicked()
            {
                state.migration.step = Step::Adopt;
            }
        }
        let lost = state.migration.active_lost().len();
        if ui
            .add_enabled(lost > 0, egui::Button::new(trf("Lost files ({n})", &[("n", &lost)])))
            .clicked()
        {
            state.migration.step = Step::Lost;
        }
        if ui
            .add_enabled(
                !state.migration.status_loading(),
                egui::Button::new(tr("Refresh")),
            )
            .clicked()
        {
            state.migration.start_status(&state.settings.rclone);
        }
        let complete = status.as_ref().is_some_and(|s| s.complete && !s.abandoned);
        if ui
            .add_enabled(complete, egui::Button::new(tr("Clean up")))
            .on_hover_text(tr("After completion: free the space of the replaced originals and of partial copies (quarantine first, permanent deletion after a grace period)."))
            .clicked()
        {
            state.migration.step = Step::Cleanup;
        }
        if ui.button(tr("Back to plan")).clicked() {
            state.migration.reset_to_plan();
        }
    });
    if drive.is_none() {
        theme::hint(
            ui,
            tr("No drive part: this pool has no drive, or it was left out of the plan."),
        );
    }
}

/// Height factor of the existing-migrations list.
const EXISTING_SCALE: f32 = 3.0;

/// Migrations of the selected pool found in the cloud.
pub(super) fn existing(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(tr("Existing migrations"));
        let loading = state.migration.status_loading();
        if ui
            .add_enabled(
                !loading && !state.migration.pool.is_empty(),
                egui::Button::new(tr("Refresh")),
            )
            .clicked()
        {
            state.migration.start_status(&state.settings.rclone);
        }
        if loading {
            ui.spinner();
        }
    });
    if let Some(error) = &state.migration.status_error {
        ui.colored_label(ui.visuals().warn_fg_color, error);
    }
    if state.migration.statuses.is_empty() {
        theme::hint(
            ui,
            tr("No migration of this pool is recorded in the cloud."),
        );
        return;
    }
    let idle = !task.is_running();
    let rows = state.migration.statuses.clone();
    // Three times the rows' height (and cap): one or two migrations in a
    // one-line box were hard to read.
    let height = (EXISTING_SCALE * (rows.len() as f32 * (theme::ROW_HEIGHT + 4.0) + 40.0))
        .min(EXISTING_SCALE * theme::list_height(ui.ctx().content_rect().height()));
    let size = egui::vec2(ui.available_width(), height);
    let mut action: Option<(u8, String)> = None;
    theme::fixed_pane_wide(ui, "migration-existing", size, |ui| {
        egui::Grid::new("migration-existing-grid")
            .striped(true)
            .num_columns(6)
            .show(ui, |ui| {
                for head in [
                    tr("Id"),
                    tr("Created"),
                    tr("Progress"),
                    tr("Lost"),
                    tr("State"),
                    "",
                ] {
                    ui.strong(head);
                }
                ui.end_row();
                for status in &rows {
                    ui.monospace(short_id(&status.migration_id))
                        .on_hover_text(&status.migration_id);
                    ui.label(format!(
                        "{} · {}",
                        status.created_by,
                        crate::gui::i18n::relative_age(status.created_unix)
                    ));
                    let drive = state.migration.drive_statuses.get(&status.migration_id);
                    match drive {
                        Some(drive) => ui.label(format!(
                            "{} · {}",
                            progress_text(status),
                            super::drive_part::drive_state(drive).0
                        )),
                        None => ui.label(progress_text(status)),
                    };
                    ui.label(status.lost.len().to_string());
                    let (label, tone) = state_label(status);
                    status_badge(ui, label, tone);
                    ui.horizontal(|ui| {
                        let open = !status.abandoned && !status.complete;
                        if ui
                            .add_enabled(idle && open, egui::Button::new(tr("Resume")))
                            .clicked()
                        {
                            action = Some((0, status.migration_id.clone()));
                        }
                        if ui.button(tr("Open")).clicked() {
                            action = Some((1, status.migration_id.clone()));
                        }
                        if ui
                            .add_enabled(
                                idle && !status.abandoned,
                                egui::Button::new(tr("Discard")),
                            )
                            .clicked()
                        {
                            action = Some((2, status.migration_id.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
    });
    let rclone = state.settings.rclone.clone();
    let result = match action {
        Some((0, id)) => state.migration.start_run(task, &rclone, &id),
        Some((1, id)) => {
            state.migration.open(&id);
            Ok(())
        }
        Some((_, id)) => state.migration.start_abandon(task, &rclone, &id),
        None => Ok(()),
    };
    if let Err(error) = result {
        state.migration.error = Some(error);
    }
}
