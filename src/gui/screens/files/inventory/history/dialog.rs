//! A modal dialog frame: title, body, never wider than the window.

use crate::gui::theme;
use eframe::egui;

/// Shows `body` in a modal titled `title`, `width` wide at most. Returns the
/// body's value and whether the dialog should close (Escape or a click on
/// the backdrop).
pub(crate) fn modal<R>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let screen = ctx.content_rect();
    let width = width.min(screen.width() - 48.0).max(200.0);
    let max_height = (screen.height() - 64.0).max(160.0);
    let response = egui::Modal::new(egui::Id::new(("history-dialog", id))).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_height(max_height);
        ui.label(
            egui::RichText::new(title)
                .size(theme::CARD_TITLE_SIZE)
                .strong()
                .color(theme::pal(ui).text),
        );
        ui.add_space(theme::SUBSECTION_GAP);
        body(ui)
    });
    let close = response.should_close();
    (response.inner, close)
}
