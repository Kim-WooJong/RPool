use super::state::IntegrityForm;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::relative_age;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, form: &IntegrityForm) {
    ui.label(egui::RichText::new("Last integrity result").strong());
    ui.add_space(theme::SUBSECTION_GAP);

    let Some(snapshot) = &form.snapshot else {
        status_badge(ui, "Not checked", StatusTone::Neutral);
        ui.label(egui::RichText::new("Run a scrub to establish current archive integrity.").weak());
        return;
    };

    let tone = if snapshot.errors > 0 || snapshot.unrecoverable_groups > 0 {
        StatusTone::Error
    } else if snapshot.total != snapshot.healthy {
        StatusTone::Warning
    } else {
        StatusTone::Success
    };
    let label = if snapshot.errors > 0 {
        "Provider error"
    } else if snapshot.unrecoverable_groups > 0 {
        "Unrecoverable"
    } else if snapshot.total != snapshot.healthy {
        "Degraded"
    } else {
        "Healthy"
    };
    status_badge(ui, label, tone);

    egui::Grid::new("integrity-summary-grid")
        .num_columns(4)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            item(ui, "Healthy", snapshot.healthy);
            item(ui, "Missing", snapshot.missing);
            item(ui, "Bad size", snapshot.bad_size);
            item(ui, "Corrupt", snapshot.corrupt);
            ui.end_row();
            item(ui, "Provider errors", snapshot.errors);
            item(
                ui,
                "Recoverable",
                snapshot
                    .groups
                    .iter()
                    .filter(|group| group.is_recoverable())
                    .count(),
            );
            item(ui, "Unrecoverable", snapshot.unrecoverable_groups);
            item(ui, "Degraded groups", snapshot.degraded_groups);
            ui.end_row();
            item(ui, "Total shards", snapshot.total);
        });

    ui.small(format!(
        "Archive {} · {} · checked {}",
        snapshot.archive_id,
        snapshot.mode,
        relative_age(snapshot.checked_unix)
    ));
}

fn item(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.vertical(|ui| {
        ui.small(egui::RichText::new(label).weak());
        ui.label(egui::RichText::new(value.to_string()).strong());
    });
}
