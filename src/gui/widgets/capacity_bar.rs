use crate::gui::theme;
use eframe::egui;

pub(crate) fn capacity_bar_sized(
    ui: &mut egui::Ui,
    ratio: Option<f32>,
    text: &str,
    width: f32,
) {
    let ratio = ratio.map(|value| value.clamp(0.0, 1.0));
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width.max(1.0), theme::CAPACITY_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(theme::CORNER_RADIUS);

    painter.rect_filled(rect, radius, ui.visuals().widgets.inactive.bg_fill);

    if let Some(ratio) = ratio {
        let filled_width = rect.width() * ratio;
        if filled_width > 0.0 {
            let filled = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.left() + filled_width, rect.bottom()),
            );
            painter.rect_filled(filled, radius, ui.visuals().selection.bg_fill);
        }
    }

    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        ui.visuals().text_color(),
    );
}
