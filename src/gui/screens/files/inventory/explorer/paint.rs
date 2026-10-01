//! Painting single-line, truncated cell text inside a row rectangle.

use eframe::egui;

const PAD: f32 = 6.0;

/// Paints `text` in the column `[left, left + width)` of `row`, cut with
/// "…" when it does not fit; right-aligned when `right` is set.
pub(crate) fn cell(
    ui: &egui::Ui,
    row: egui::Rect,
    left: f32,
    width: f32,
    text: egui::RichText,
    right: bool,
) {
    let inner = (width - 2.0 * PAD).max(1.0);
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        inner,
        egui::TextStyle::Body,
    );
    let x = if right {
        left + width - PAD - galley.size().x
    } else {
        left + PAD
    };
    let pos = egui::pos2(x, row.center().y - galley.size().y / 2.0);
    let clip = egui::Rect::from_x_y_ranges(left..=left + width, row.y_range());
    ui.painter()
        .with_clip_rect(clip.intersect(ui.clip_rect()))
        .galley(pos, galley, ui.visuals().text_color());
}
