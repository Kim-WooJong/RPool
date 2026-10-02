//! Small charts drawn with the egui painter: the live upload/download
//! sparkline and the mirrored history bars (upload above the axis, download
//! below).
use super::history::RemoteHistory;
use super::ring::{unit_points, Ring};
use crate::gui::theme;
use eframe::egui;

/// Height of an account's live sparkline, in points.
pub(crate) const SPARKLINE_HEIGHT: f32 = 46.0;
/// Height of an account's history bar chart, in points.
pub(crate) const BARS_HEIGHT: f32 = 64.0;

/// Upload and download colours (the success and info foregrounds).
pub(crate) fn colors(ui: &egui::Ui) -> (egui::Color32, egui::Color32) {
    let dark = ui.visuals().dark_mode;
    (theme::success_colors(dark).1, theme::info_colors(dark).1)
}

/// Allocates a full-width chart area of `height` and paints its background
/// and border; returns the rectangle to draw into.
fn frame(ui: &mut egui::Ui, height: f32) -> egui::Rect {
    let p = theme::pal(ui);
    let width = ui.available_width().max(40.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(theme::CORNER_RADIUS),
        p.surface_alt,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    rect
}

/// Upload and download rates of the last minutes, scaled to their peak.
pub(crate) fn sparkline(ui: &mut egui::Ui, ring: Option<&Ring>, now: u64) {
    let rect = frame(ui, SPARKLINE_HEIGHT);
    let Some(ring) = ring.filter(|ring| ring.len() > 1) else {
        return;
    };
    let (up_color, down_color) = colors(ui);
    let inner = rect.shrink2(egui::vec2(6.0, 5.0));
    let peak = ring.peak();
    let to_screen = |[x, y]: [f32; 2]| {
        egui::pos2(
            inner.left() + x * inner.width(),
            inner.bottom() - y * inner.height(),
        )
    };
    for (series, color) in [
        (unit_points(ring, now, peak, |s| s.down), down_color),
        (unit_points(ring, now, peak, |s| s.up), up_color),
    ] {
        let points: Vec<egui::Pos2> = series.into_iter().map(to_screen).collect();
        if points.len() > 1 {
            ui.painter()
                .add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
        }
    }
}

/// One account's history buckets: upload bars up from the middle line,
/// download bars down, both scaled to the account's busiest bucket.
pub(crate) fn history_bars(ui: &mut egui::Ui, history: &RemoteHistory) {
    let rect = frame(ui, BARS_HEIGHT);
    let p = theme::pal(ui);
    let (up_color, down_color) = colors(ui);
    let inner = rect.shrink2(egui::vec2(6.0, 4.0));
    let middle = inner.center().y;
    ui.painter().line_segment(
        [
            egui::pos2(inner.left(), middle),
            egui::pos2(inner.right(), middle),
        ],
        egui::Stroke::new(1.0, p.border),
    );
    let peak = history.peak();
    let count = history.buckets.len();
    if peak == 0 || count == 0 {
        return;
    }
    let step = inner.width() / count as f32;
    let width = (step - 1.0).max(1.0);
    let half = inner.height() / 2.0 - 1.0;
    for (index, bucket) in history.buckets.iter().enumerate() {
        let left = inner.left() + index as f32 * step;
        for (value, color, up) in [
            (bucket.up, up_color, true),
            (bucket.down, down_color, false),
        ] {
            if value == 0 {
                continue;
            }
            // At least one pixel so small traffic stays visible.
            let height = (value as f32 / peak as f32 * half).max(1.0);
            let (top, bottom) = if up {
                (middle - height, middle)
            } else {
                (middle, middle + height)
            };
            ui.painter().rect_filled(
                egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + width, bottom)),
                egui::CornerRadius::ZERO,
                color,
            );
        }
    }
}
