//! Badges of the history views: version kinds, "Current", and notices.

use crate::drive_history::model::VersionKind;
use crate::gui::i18n::tr;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// Translated name of a version kind; also used by the migration cleanup step.
pub(crate) fn kind_label(kind: VersionKind) -> &'static str {
    match kind {
        VersionKind::Created => tr("Created"),
        VersionKind::Modified => tr("Modified"),
        VersionKind::Deleted => tr("Deleted"),
        VersionKind::Restored => tr("Restored"),
    }
}

/// A version-kind badge (Created / Modified / Deleted / Restored) in its tone.
/// Used by the versions panel rows.
pub(crate) fn kind(ui: &mut egui::Ui, kind: VersionKind) {
    let tone = match kind {
        VersionKind::Created => StatusTone::Info,
        VersionKind::Modified => StatusTone::Neutral,
        VersionKind::Deleted => StatusTone::Error,
        VersionKind::Restored => StatusTone::Warning,
    };
    status_badge(ui, kind_label(kind), tone);
}

/// The "Current" badge marking the live version in the versions panel.
pub(crate) fn current(ui: &mut egui::Ui) {
    status_badge(ui, tr("Current"), StatusTone::Success);
}

/// The result of the last change, in success or error colours.
pub(crate) fn notice(ui: &mut egui::Ui, success: bool, text: &str) {
    let dark = ui.visuals().dark_mode;
    let colors = if success {
        theme::success_colors(dark)
    } else {
        theme::error_colors(dark)
    };
    banner(ui, colors, text);
}

/// A highlighted explanation (info colours).
pub(crate) fn info(ui: &mut egui::Ui, text: &str) {
    banner(ui, theme::info_colors(ui.visuals().dark_mode), text);
}

/// A warning (warning colours), e.g. before a permanent deletion.
pub(crate) fn warning(ui: &mut egui::Ui, text: &str) {
    banner(ui, theme::warning_colors(ui.visuals().dark_mode), text);
}

/// A full-width coloured frame with `text`; `(fill, fg)` from the theme.
fn banner(ui: &mut egui::Ui, (fill, fg): (egui::Color32, egui::Color32), text: &str) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(fg, text);
        });
}
