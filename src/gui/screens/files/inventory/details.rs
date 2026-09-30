use super::data::InventoryRow;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::{format_bytes, relative_age};
use eframe::egui;

#[derive(Debug, Clone, Copy)]
pub(crate) enum InventoryAction {
    Restore,
    Verify,
    Status,
}

pub(crate) fn show(ui: &mut egui::Ui, row: Option<&InventoryRow>) -> Option<InventoryAction> {
    ui.label(egui::RichText::new("Details").strong());
    ui.add_space(theme::SUBSECTION_GAP);

    let Some(row) = row else {
        ui.label(egui::RichText::new("Select a file to inspect it.").weak());
        return None;
    };

    ui.label(egui::RichText::new(&row.entry.original_name).strong());
    ui.add_space(theme::SUBSECTION_GAP);

    detail_row(ui, "Size", &format_bytes(row.entry.original_size));
    detail_row(ui, "Pool", row.pool_label());
    detail_row(ui, "Coding", &row.coding_label());
    detail_row(ui, "Created", &relative_age(row.entry.created_unix));
    detail_row(ui, "Remotes", &row.entry.remotes.len().to_string());

    ui.horizontal_wrapped(|ui| {
        ui.label("Health");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            status_badge(ui, "Not checked", StatusTone::Neutral);
        });
    });
    ui.label(
        egui::RichText::new(
            "Inventory stores metadata only. Run Status or Verify for current remote health.",
        )
        .small()
        .weak(),
    );

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SECTION_GAP);

    if ui.button("Restore…").clicked() {
        return Some(InventoryAction::Restore);
    }
    if ui.button("Verify…").clicked() {
        return Some(InventoryAction::Verify);
    }
    if ui.button("Check status…").clicked() {
        return Some(InventoryAction::Status);
    }

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(egui::RichText::new("Archive ID").strong());
    ui.monospace(&row.entry.archive_id);
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(egui::RichText::new("Manifest").strong());
    ui.label(&row.entry.manifest_source);

    if !row.entry.remotes.is_empty() {
        ui.add_space(theme::SUBSECTION_GAP);
        ui.label(egui::RichText::new("Storage remotes").strong());
        for remote in &row.entry.remotes {
            ui.monospace(remote);
        }
    }

    None
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(value);
        });
    });
}
