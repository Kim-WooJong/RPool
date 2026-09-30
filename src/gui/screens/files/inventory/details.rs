use super::data::InventoryRow;
use crate::gui::i18n::relative_age;
use crate::gui::i18n::tr;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::format_bytes;
use eframe::egui;

#[derive(Debug, Clone, Copy)]
pub(crate) enum InventoryAction {
    Restore,
    Verify,
    Status,
}

pub(crate) fn show(ui: &mut egui::Ui, row: Option<&InventoryRow>) -> Option<InventoryAction> {
    ui.label(egui::RichText::new(tr("Details")).strong());
    ui.add_space(theme::SUBSECTION_GAP);

    let Some(row) = row else {
        ui.label(egui::RichText::new(tr("Select a file to inspect it.")).weak());
        return None;
    };

    ui.label(egui::RichText::new(&row.entry.original_name).strong());
    ui.add_space(theme::SUBSECTION_GAP);

    detail_row(ui, tr("Size"), &format_bytes(row.entry.original_size));
    detail_row(ui, tr("Pool"), row.pool_label());
    detail_row(ui, tr("Coding"), &row.coding_label());
    detail_row(ui, tr("Created"), &relative_age(row.entry.created_unix));
    detail_row(ui, tr("Remotes"), &row.entry.remotes.len().to_string());

    ui.horizontal_wrapped(|ui| {
        ui.label(tr("Health"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            status_badge(ui, tr("Not checked"), StatusTone::Neutral);
        });
    });
    ui.label(
        egui::RichText::new(tr(
            "Inventory stores metadata only. Run Status or Verify for current remote health.",
        ))
        .small()
        .weak(),
    );

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SECTION_GAP);

    if ui.button(tr("Restore…")).clicked() {
        return Some(InventoryAction::Restore);
    }
    if ui.button(tr("Verify…")).clicked() {
        return Some(InventoryAction::Verify);
    }
    if ui.button(tr("Check status…")).clicked() {
        return Some(InventoryAction::Status);
    }

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(egui::RichText::new(tr("Archive ID")).strong());
    ui.monospace(&row.entry.archive_id);
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(egui::RichText::new(tr("Manifest")).strong());
    ui.label(&row.entry.manifest_source);

    if !row.entry.remotes.is_empty() {
        ui.add_space(theme::SUBSECTION_GAP);
        ui.label(egui::RichText::new(tr("Storage remotes")).strong());
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
