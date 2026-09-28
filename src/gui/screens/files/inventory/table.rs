use super::data::InventoryRow;
use super::state::{InventoryForm, InventorySort};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::{format_bytes, relative_age};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, rows: &[InventoryRow], form: &mut InventoryForm) {
    if rows.is_empty() {
        ui.add_space(theme::SECTION_GAP);
        ui.label("No files match the current filters.");
        return;
    }

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("inventory-file-table")
                .num_columns(6)
                .striped(true)
                .spacing([16.0, 7.0])
                .show(ui, |ui| {
                    sort_header(ui, form, InventorySort::Name, "Name");
                    sort_header(ui, form, InventorySort::Size, "Size");
                    sort_header(ui, form, InventorySort::Pool, "Pool");
                    sort_header(ui, form, InventorySort::Coding, "Coding");
                    ui.strong("Health");
                    sort_header(ui, form, InventorySort::Created, "Created");
                    ui.end_row();

                    for row in rows {
                        let selected = form.selected_archive_id.as_deref()
                            == Some(row.entry.archive_id.as_str());
                        if ui
                            .selectable_label(selected, row.entry.original_name.as_str())
                            .clicked()
                        {
                            form.selected_archive_id = Some(row.entry.archive_id.clone());
                        }
                        ui.monospace(format_bytes(row.entry.original_size));
                        ui.label(row.pool_label());
                        ui.monospace(row.coding_label());
                        status_badge(ui, "Not checked", StatusTone::Neutral);
                        ui.monospace(relative_age(row.entry.created_unix));
                        ui.end_row();
                    }
                });
        });
}

fn sort_header(ui: &mut egui::Ui, form: &mut InventoryForm, sort: InventorySort, label: &str) {
    let text = form.sort_label(sort, label);
    if ui.link(text).clicked() {
        form.set_sort(sort);
    }
}
