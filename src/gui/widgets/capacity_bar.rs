use crate::gui::theme;
use eframe::egui;

/// Usage bar with a centered label whose colour flips at the fill edge:
/// over the filled part it contrasts with the accent fill, over the empty
/// track it contrasts with the track.
pub(crate) fn capacity_bar_sized(ui: &mut egui::Ui, ratio: Option<f32>, text: &str, width: f32) {
    let p = theme::pal(ui);
    capacity_bar_colored(ui, ratio, text, width, p.accent, p.accent_fg);
}

/// [`capacity_bar_sized`] with its own fill colour and the label colour over
/// the fill (e.g. a warning colour for the slowest account of a speed test).
pub(crate) fn capacity_bar_colored(
    ui: &mut egui::Ui,
    ratio: Option<f32>,
    text: &str,
    width: f32,
    fill: egui::Color32,
    fill_text: egui::Color32,
) {
    let ratio = ratio.map(|value| value.clamp(0.0, 1.0));
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width.max(1.0), theme::CAPACITY_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    let p = theme::pal(ui);
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(theme::CORNER_RADIUS);
    let font = egui::FontId::proportional(12.0);

    painter.rect_filled(rect, radius, p.surface_alt);

    let filled_width = ratio.map_or(0.0, |ratio| rect.width() * ratio);
    if filled_width <= 0.0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font,
            p.text,
        );
        return;
    }

    // Paint the full rounded track shape clipped to the filled region, so the
    // left edge keeps the track rounding and the right edge is a straight cut
    // that stays inside the track even for tiny ratios.
    let split = rect.left() + filled_width;
    let filled = egui::Rect::from_min_max(rect.min, egui::pos2(split, rect.bottom()));
    let empty = egui::Rect::from_min_max(egui::pos2(split, rect.top()), rect.max);
    let filled_painter = painter.with_clip_rect(filled);
    filled_painter.rect_filled(rect, radius, fill);

    filled_painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        font.clone(),
        fill_text,
    );
    painter.with_clip_rect(empty).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        font,
        p.text,
    );
}

#[cfg(test)]
mod tests {
    use super::capacity_bar_sized;
    use eframe::egui;

    #[test]
    fn capacity_bar_renders_all_ratios_in_both_themes() {
        for dark in [true, false] {
            let ctx = egui::Context::default();
            ctx.set_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            crate::gui::theme::apply(&ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    for ratio in [None, Some(0.0), Some(0.02), Some(0.56), Some(1.0)] {
                        capacity_bar_sized(ui, ratio, "56%", 120.0);
                    }
                    capacity_bar_sized(ui, Some(0.5), "tiny", 1.0);
                });
            });
            output.textures_delta.clear();
        }
    }
}
