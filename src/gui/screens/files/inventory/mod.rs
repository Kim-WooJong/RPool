mod data;
mod details;
mod filters;
mod rebuild;
mod state;
mod table;

pub(crate) use state::InventoryForm;

use crate::gui::state::{FilesSection, GuiState};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{section_header, toolbar};
use crate::presentation::format_bytes;
use details::InventoryAction;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    if !state.inventory.loaded {
        state.inventory.refresh();
    }

    section_header(
        ui,
        "Files",
        Some("Browse the local inventory index and open archive operations from one place."),
    );

    let mut switch_to_upload = false;
    toolbar(ui, |ui| {
        if ui.button("Upload").clicked() {
            switch_to_upload = true;
        }
        if ui.button("Refresh").clicked() {
            state.inventory.refresh();
        }
        if ui.button("Rebuild…").clicked() {
            state.inventory.show_rebuild = !state.inventory.show_rebuild;
        }
    });

    if switch_to_upload {
        state.files_section = FilesSection::Upload;
        return;
    }

    if state.inventory.show_rebuild {
        ui.add_space(theme::SUBSECTION_GAP);
        rebuild::show(ui, &mut state.inventory, task, &state.settings.rclone);
    }

    ui.add_space(theme::SUBSECTION_GAP);
    filters::show(ui, &mut state.inventory);

    if let Some(error) = &state.inventory.error {
        ui.add_space(theme::SUBSECTION_GAP);
        ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
    }

    let rows = state.inventory.visible_rows();
    let visible_bytes = rows
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.entry.original_size));
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(
        egui::RichText::new(format!(
            "{} file{} · {}",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" },
            format_bytes(visible_bytes)
        ))
        .weak(),
    );

    ui.add_space(theme::SUBSECTION_GAP);
    let selected = state
        .inventory
        .selected_archive_id
        .as_deref()
        .and_then(|selected| rows.iter().find(|row| row.entry.archive_id == selected))
        .cloned();
    let mut action = None;
    let total_width = ui.available_width();
    let detail_width = 300.0_f32.min((total_width * 0.38).max(250.0));
    let table_width = (total_width - detail_width - theme::SECTION_GAP).max(360.0);

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(table_width);
            table::show(ui, &rows, &mut state.inventory);
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(detail_width);
            egui::ScrollArea::vertical()
                .id_salt("inventory-details-scroll")
                .show(ui, |ui| {
                    action = details::show(ui, selected.as_ref());
                });
        });
    });

    if let (Some(action), Some(row)) = (action, selected) {
        match action {
            InventoryAction::Restore => {
                state.restore.manifest = row.entry.manifest_source;
                state.files_section = FilesSection::Restore;
            }
            InventoryAction::Verify => {
                state.verify.manifest = row.entry.manifest_source;
                state.files_section = FilesSection::Verify;
            }
            InventoryAction::Status => {
                state.status.manifest = row.entry.manifest_source;
                state.files_section = FilesSection::Status;
            }
        }
    }
}
