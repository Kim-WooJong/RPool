use super::state::IntegrityForm;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, form: &IntegrityForm) {
    let Some(snapshot) = &form.snapshot else {
        return;
    };
    if snapshot.issues.is_empty() {
        return;
    }

    ui.collapsing(format!("Issues ({})", snapshot.issues.len()), |ui| {
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                egui::Grid::new("integrity-issues-grid")
                    .striped(true)
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        ui.strong("Shard");
                        ui.strong("Group");
                        ui.strong("Remote");
                        ui.strong("Status");
                        ui.strong("Detail");
                        ui.end_row();
                        for issue in &snapshot.issues {
                            ui.monospace(format!("{:08}", issue.index));
                            ui.label(issue.group.to_string());
                            ui.monospace(&issue.remote);
                            let tone = if issue.status == "error" {
                                StatusTone::Error
                            } else {
                                StatusTone::Warning
                            };
                            status_badge(ui, &issue.status, tone);
                            ui.label(issue.detail.as_deref().unwrap_or("—"));
                            ui.end_row();
                        }
                    });
            });
    });
}
