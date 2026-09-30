mod advanced;
mod batch;
mod file_queue;
mod manual_options;
mod start;
mod state;
mod summary;
mod target;
mod validation;

pub(crate) use state::UploadForm;

use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{section_header, toolbar};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_header(
        ui,
        "Upload",
        Some("Archive local files using a storage pool. Files are uploaded sequentially and remain encrypted through rclone crypt destinations."),
    );

    // Reserve the action row before dividing the remaining viewport into panes.
    let gap = ui.spacing().item_spacing;
    let available = ui.available_size();
    let body_height =
        (available.y - crate::gui::theme::ROW_HEIGHT.max(ui.spacing().interact_size.y) - gap.y)
            .max(1.0);
    let wide = crate::gui::theme::wide(ui);
    let row_height = if wide {
        ((body_height - gap.y) / 2.0).max(200.0)
    } else {
        260.0
    };
    crate::gui::theme::page_body(ui, "upload", |ui| {
        crate::gui::theme::fixed_pane(
            ui,
            "upload-sources-scroll",
            egui::vec2(available.x, row_height),
            |ui| {
                file_queue::show(ui, &mut state.upload);
            },
        );

        // Calculate preflight after rendering the destination controls so edits are
        // reflected in the summary and start-button state in this same frame.
        let column_width = if wide {
            ((available.x - gap.x) / 2.0).max(1.0)
        } else {
            available.x
        };
        let row = if wide {
            egui::Layout::left_to_right(egui::Align::Min)
        } else {
            egui::Layout::top_down(egui::Align::Min)
        };
        let preflight = ui
            .with_layout(row, |ui| {
                crate::gui::theme::fixed_pane(
                    ui,
                    "upload-options-scroll",
                    egui::vec2(column_width, row_height),
                    |ui| {
                        target::show_primary(ui, state);
                        ui.add_space(8.0);
                        advanced::show(ui, state);
                    },
                );
                let evaluated = validation::evaluate(state);
                crate::gui::theme::fixed_pane(
                    ui,
                    "upload-preflight-scroll",
                    egui::vec2(column_width, row_height),
                    |ui| {
                        summary::show(ui, &evaluated);
                        if let Some(error) = &state.upload.error {
                            ui.add_space(6.0);
                            let (_, foreground) =
                                crate::gui::theme::error_colors(ui.visuals().dark_mode);
                            ui.label(egui::RichText::new(error).color(foreground));
                        }
                    },
                );
                evaluated
            })
            .inner;

        toolbar(ui, |ui| {
            let pending = state.upload.pending_count();
            let label = if state.upload.batch_active {
                let position = state
                    .upload
                    .active_index
                    .map(|index| index + 1)
                    .unwrap_or_else(|| {
                        (state.upload.completed_count() + 1).min(state.upload.items.len())
                    });
                format!("Uploading {position} / {}", state.upload.items.len())
            } else if pending > 1 {
                format!("Upload {pending} files")
            } else {
                "Start upload".to_string()
            };
            let can_start =
                !task.is_running() && !state.upload.batch_active && pending > 0 && preflight.ready;
            let response = ui.add_enabled(can_start, egui::Button::new(label));
            let response = if !preflight.ready {
                response.on_hover_text(
                    preflight
                        .first_issue()
                        .unwrap_or("Review the upload configuration before starting."),
                )
            } else {
                response
            };
            if response.clicked() {
                state.upload.error = batch::start_batch(state, task).err();
            }
        });
    });
}

pub(crate) fn poll_batch(state: &mut GuiState, task: &mut TaskRunner) {
    batch::poll_batch(state, task);
}
