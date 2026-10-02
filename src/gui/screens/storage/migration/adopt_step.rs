//! Step 4: adopt the migrated drive (`pool migrate adopt`): it becomes the
//! pool's drive on every PC. Optionally switches this PC's drive workspace
//! (kept as a backup; local-only writes exported).
use super::drive_part::{drive_state, generation_text};
use super::state::{short_id, Step, Watched};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::status_badge;
use eframe::egui;

/// Renders step 4 (adopt) of the migration wizard for the active migration;
/// falls back to the plan step when no migration is active.
pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let Some(id) = state.migration.active_id.clone() else {
        state.migration.reset_to_plan();
        return;
    };
    if state.migration.workspace.is_empty() {
        if let Some(workspace) = state.mount.workspace_of(&state.migration.pool) {
            state.migration.workspace = workspace;
        }
    }
    let running = task.is_running() && state.migration.watched == Some(Watched::Adopt(id.clone()));
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(trf(
                "Adopt the drive of migration {id}",
                &[("id", &short_id(&id))],
            ))
            .strong(),
        );
        if state.migration.status_loading() {
            ui.spinner();
        }
    });
    let Some(drive) = state.migration.active_drive_status().cloned() else {
        theme::hint(
            ui,
            state.migration.status_error.as_deref().unwrap_or(tr(
                "This migration has no drive part, or its progress is still loading.",
            )),
        );
        if ui.button(tr("Back")).clicked() {
            state.migration.step = Step::Run;
        }
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(trf(
            "From {source} to layout {epoch}",
            &[
                ("source", &generation_text(&drive.source)),
                ("epoch", &short_id(&drive.epoch)),
            ],
        ));
        let (label, tone) = drive_state(&drive);
        status_badge(ui, label, tone);
    });
    if let Some(adopted) = &drive.adopted {
        theme::hint(
            ui,
            &trf(
                "Adopted by {pc}: {files} files in the new layout, {dropped} unrecoverable left out.",
                &[
                    ("pc", &adopted.pc_id),
                    ("files", &adopted.files),
                    ("dropped", &adopted.dropped.len()),
                ],
            ),
        );
    } else if !drive.ready {
        theme::hint(ui, tr("Some drive files have no new archive yet. Adoption catches them up (and files changed since planning), but finishing the run first is faster."));
    }
    if !drive.bootstrap_ok {
        ui.colored_label(
            ui.visuals().error_fg_color,
            tr("The drive has too many files for a new PC to open in one go; its adoption will be refused. Archives still migrate."),
        );
    }
    let lost = drive.lost.len();
    ui.add_enabled_ui(!running, |ui| {
        if lost > 0 {
            ui.checkbox(
                &mut state.migration.accept_lost,
                trf("Leave the {n} unrecoverable drive files out", &[("n", &lost)]),
            )
            .on_hover_text(tr("Without this, adoption stops and lists them. They are never deleted; the previous layout keeps their records."));
        }
        ui.checkbox(
            &mut state.migration.switch_workspace,
            tr("Also switch this PC's drive workspace"),
        )
        .on_hover_text(tr("Unmount the drive first. The workspace is renamed to a backup next to it (changes that were only on this PC are exported there), and the next mount opens the new layout."));
        if state.migration.switch_workspace {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Workspace"));
                ui.add(
                    egui::TextEdit::singleline(&mut state.migration.workspace)
                        .desired_width(ui.available_width().min(420.0)),
                );
            });
        }
        ui.checkbox(&mut state.migration.take_over, tr("Take over work of a stopped PC"));
    });
    theme::hint(ui, tr("Adoption waits until PCs on the current drive have stopped uploading (about 5 minutes after the drive part started), then publishes the new layout and checks it reads back. Nothing is deleted; the previous layout stays as it is."));
    let idle = !task.is_running();
    ui.horizontal_wrapped(|ui| {
        let can = idle
            && drive.adopted.is_none()
            && drive.bootstrap_ok
            && (!state.migration.switch_workspace || !state.migration.workspace.trim().is_empty());
        if theme::primary_button(ui, can, tr("Adopt the new drive layout")).clicked() {
            if let Err(error) = state
                .migration
                .start_adopt(task, &state.settings.rclone, &id)
            {
                state.migration.error = Some(error);
            }
        }
        if drive.adopted.is_some()
            && theme::primary_button(
                ui,
                idle && !state.migration.workspace.trim().is_empty(),
                tr("Switch this PC's drive workspace"),
            )
            .clicked()
        {
            state.migration.switch_workspace = true;
            if let Err(error) = state
                .migration
                .start_adopt(task, &state.settings.rclone, &id)
            {
                state.migration.error = Some(error);
            }
        }
        if running {
            ui.spinner();
            if ui
                .add_enabled(!state.migration.pausing, egui::Button::new(tr("Pause")))
                .clicked()
            {
                if let Err(error) = state.migration.request_pause() {
                    state.migration.error = Some(error);
                }
            }
        }
        if ui
            .add_enabled(
                lost > 0,
                egui::Button::new(trf("Lost files ({n})", &[("n", &lost)])),
            )
            .clicked()
        {
            state.migration.step = Step::Lost;
        }
        if ui.button(tr("Back")).clicked() {
            state.migration.step = Step::Run;
        }
    });
    if running {
        if let Some(line) = task.logs().last() {
            theme::hint(ui, &line.text);
        }
    }
}
