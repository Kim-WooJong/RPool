//! The source-file queue of Files › Upload: drop zone, Add files… / Clear, and
//! one row per file with size, status and Remove.

use super::state::{UploadForm, UploadItemStatus};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::format_bytes;
use eframe::egui;
use std::path::PathBuf;

/// Draws the queue and adds dropped or picked files to `form`. Editing is
/// disabled while a batch runs. Called by `upload::show`.
pub(crate) fn show(ui: &mut egui::Ui, form: &mut UploadForm) {
    collect_dropped_files(ui, form);

    let hovering_files = ui.ctx().input(|input| !input.raw.hovered_files.is_empty());
    let fill = if hovering_files {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        ui.visuals().faint_bg_color
    };

    egui::Frame::NONE
        .fill(fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(tr("Source files")).strong());
                    ui.label(
                        egui::RichText::new(tr(
                            "Drop local files here or add several files at once.",
                        ))
                        .weak(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!form.batch_active, egui::Button::new(tr("Clear")))
                        .clicked()
                    {
                        form.clear_queue();
                    }
                    if ui.button(tr("Add files…")).clicked() {
                        if let Some(paths) = rfd::FileDialog::new().pick_files() {
                            form.add_paths(paths);
                        }
                    }
                });
            });
        });

    ui.add_space(6.0);
    if form.items.is_empty() {
        ui.label(egui::RichText::new(tr("No files selected.")).weak());
        return;
    }

    let mut remove_index = None;
    egui::Grid::new("upload-file-queue-grid")
        .num_columns(4)
        .striped(true)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.strong(tr("File"));
            ui.strong(tr("Size"));
            ui.strong(tr("Status"));
            ui.label("");
            ui.end_row();

            for (index, item) in form.items.iter().enumerate() {
                ui.label(item.file_name())
                    .on_hover_text(item.path.display().to_string());
                ui.monospace(
                    item.size
                        .map(format_bytes)
                        .unwrap_or_else(|| tr("n/a").to_string()),
                );
                let tone = match item.status {
                    UploadItemStatus::Pending => StatusTone::Neutral,
                    UploadItemStatus::Running => StatusTone::Warning,
                    UploadItemStatus::Completed => StatusTone::Success,
                    UploadItemStatus::Failed | UploadItemStatus::Cancelled => StatusTone::Error,
                };
                status_badge(ui, item.status.label(), tone);
                if ui
                    .add_enabled(!form.batch_active, egui::Button::new(tr("Remove")))
                    .clicked()
                {
                    remove_index = Some(index);
                }
                ui.end_row();
            }
        });

    if let Some(index) = remove_index {
        form.items.remove(index);
    }

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(trf(
            "{n} file(s) · {size} · {pending} pending · {completed} completed",
            &[
                ("n", &form.items.len()),
                ("size", &format_bytes(form.total_bytes())),
                ("pending", &form.pending_count()),
                ("completed", &form.completed_count()),
            ],
        ))
        .weak(),
    );
}

/// Adds the files dropped onto the window this frame to the queue.
fn collect_dropped_files(ui: &egui::Ui, form: &mut UploadForm) {
    let dropped: Vec<PathBuf> = ui.ctx().input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect()
    });
    if !dropped.is_empty() {
        form.add_paths(dropped);
    }
}
