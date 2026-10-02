//! "Last integrity result" card: overall badge and shard / group counters of
//! the saved snapshot.

use super::state::IntegrityForm;
use crate::gui::i18n::relative_age;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// Draws the snapshot summary (or "Not checked"). Called by `integrity::show`.
pub(crate) fn show(ui: &mut egui::Ui, form: &IntegrityForm) {
    ui.label(egui::RichText::new(tr("Last integrity result")).strong());
    ui.add_space(theme::SUBSECTION_GAP);

    let Some(snapshot) = &form.snapshot else {
        status_badge(ui, tr("Not checked"), StatusTone::Neutral);
        ui.label(
            egui::RichText::new(tr("Run a scrub to establish current archive integrity.")).weak(),
        );
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
        tr("Provider error")
    } else if snapshot.unrecoverable_groups > 0 {
        tr("Unrecoverable")
    } else if snapshot.total != snapshot.healthy {
        tr("Degraded")
    } else {
        tr("Healthy")
    };
    status_badge(ui, label, tone);

    egui::Grid::new("integrity-summary-grid")
        .num_columns(4)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            item(ui, tr("Healthy"), snapshot.healthy);
            item(ui, tr("Missing"), snapshot.missing);
            item(ui, tr("Bad size"), snapshot.bad_size);
            item(ui, tr("Corrupt"), snapshot.corrupt);
            ui.end_row();
            item(ui, tr("Provider errors"), snapshot.errors);
            item(
                ui,
                tr("Recoverable"),
                snapshot
                    .groups
                    .iter()
                    .filter(|group| group.is_recoverable())
                    .count(),
            );
            item(ui, tr("Unrecoverable"), snapshot.unrecoverable_groups);
            item(ui, tr("Degraded groups"), snapshot.degraded_groups);
            ui.end_row();
            item(ui, tr("Total shards"), snapshot.total);
        });

    ui.small(trf(
        "Archive {id} · {mode} · checked {age}",
        &[
            ("id", &snapshot.archive_id),
            ("mode", &snapshot.mode),
            ("age", &relative_age(snapshot.checked_unix)),
        ],
    ));
}

/// One counter: small label above a bold number.
fn item(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.vertical(|ui| {
        ui.small(egui::RichText::new(label).weak());
        ui.label(egui::RichText::new(value.to_string()).strong());
    });
}
