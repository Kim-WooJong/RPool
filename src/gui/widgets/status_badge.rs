use crate::gui::theme;
use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Neutral,
    Success,
    Warning,
    Error,
    Info,
}

pub(crate) fn status_badge(ui: &mut egui::Ui, label: &str, tone: StatusTone) {
    let dark = ui.visuals().dark_mode;
    let (fill, foreground) = match tone {
        StatusTone::Neutral => theme::neutral_colors(dark),
        StatusTone::Success => theme::success_colors(dark),
        StatusTone::Warning => theme::warning_colors(dark),
        StatusTone::Error => theme::error_colors(dark),
        StatusTone::Info => theme::info_colors(dark),
    };

    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .corner_radius(egui::CornerRadius::same(255))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(theme::STATUS_TEXT_SIZE)
                    .color(foreground)
                    .strong(),
            );
        });
}
