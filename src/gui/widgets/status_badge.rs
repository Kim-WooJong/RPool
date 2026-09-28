use crate::gui::theme;
use eframe::egui;

#[derive(Clone, Copy, Debug)]
pub(crate) enum StatusTone {
    Neutral,
    Success,
    Warning,
    Error,
}

pub(crate) fn status_badge(ui: &mut egui::Ui, label: &str, tone: StatusTone) {
    let dark = ui.visuals().dark_mode;
    let (fill, foreground) = match tone {
        StatusTone::Neutral => theme::neutral_colors(dark),
        StatusTone::Success => theme::success_colors(dark),
        StatusTone::Warning => theme::warning_colors(dark),
        StatusTone::Error => theme::error_colors(dark),
    };

    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(theme::STATUS_TEXT_SIZE)
                    .color(foreground)
                    .strong(),
            );
        });
}
