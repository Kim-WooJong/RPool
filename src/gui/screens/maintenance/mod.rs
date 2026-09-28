#[path = "integrity/mod.rs"]
pub(crate) mod integrity;
#[path = "metadata/mod.rs"]
pub(crate) mod metadata;
pub(crate) mod diagnostics;

pub(crate) use diagnostics::SystemForm;
pub(crate) use integrity::IntegrityForm;
pub(crate) use metadata::ManifestForm;

use crate::gui::state::{GuiState, MaintenanceSection};
use crate::gui::task::{JobStatus, TaskRunner};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_tabs(ui, &mut state.maintenance_section);
    ui.separator();

    match state.maintenance_section {
        MaintenanceSection::Integrity => integrity::show(ui, state, task),
        MaintenanceSection::Metadata => metadata::show(ui, state, task),
        MaintenanceSection::Diagnostics => diagnostics::show(ui, state, task),
    }
}

pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    let Some(last) = task.last_task() else {
        return;
    };
    match last.name.as_str() {
        "Scrub" | "Repair" => {
            state.integrity.refresh_snapshot();
            state.integrity.notice = Some(match status {
                JobStatus::Completed => format!("{} completed; integrity summary refreshed.", last.name),
                JobStatus::Cancelled => format!("{} was cancelled.", last.name),
                JobStatus::Failed => format!("{} finished with errors; the latest available integrity snapshot was loaded.", last.name),
                _ => last.name.clone(),
            });
        }
        "Inventory rebuild" => {
            state.inventory.refresh();
            state.manifest.notice = Some(match status {
                JobStatus::Completed => "Inventory rebuild completed and Files was refreshed.".to_string(),
                JobStatus::Cancelled => "Inventory rebuild was cancelled.".to_string(),
                JobStatus::Failed => "Inventory rebuild failed. See Jobs for details.".to_string(),
                _ => "Inventory rebuild updated.".to_string(),
            });
        }
        _ => {}
    }
}

fn section_tabs(ui: &mut egui::Ui, section: &mut MaintenanceSection) {
    ui.horizontal(|ui| {
        tab(ui, section, MaintenanceSection::Integrity, "Integrity");
        tab(ui, section, MaintenanceSection::Metadata, "Metadata");
        tab(ui, section, MaintenanceSection::Diagnostics, "Diagnostics");
    });
}

fn tab(
    ui: &mut egui::Ui,
    section: &mut MaintenanceSection,
    target: MaintenanceSection,
    label: &str,
) {
    if ui.selectable_label(*section == target, label).clicked() {
        *section = target;
    }
}
