//! Back / Forward / Up buttons and the breadcrumb path bar.

use super::action::{Action, HistoryRequest};
use super::breadcrumb;
use super::nav::Nav;
use crate::gui::i18n::tr;
use eframe::egui;

/// Draws the navigation bar: Back / Forward / Up buttons (disabled when not
/// possible), the breadcrumb of `pool` and the current folder, and Roll back….
/// Returns the clicked action. Called by `explorer::show` above the list.
pub(crate) fn show(ui: &mut egui::Ui, pool: &str, nav: &Nav) -> Option<Action> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        let mut button = |ui: &mut egui::Ui, enabled, glyph: &str, hint: &str, clicked| {
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(glyph).min_size(egui::vec2(30.0, 0.0)),
                )
                .on_hover_text(hint)
                .clicked()
            {
                action = Some(clicked);
            }
        };
        button(ui, nav.can_back(), "⏴", tr("Back (Alt+Left)"), Action::Back);
        button(
            ui,
            nav.can_forward(),
            "⏵",
            tr("Forward (Alt+Right)"),
            Action::Forward,
        );
        button(
            ui,
            nav.can_up(),
            "⏶",
            tr("Up one folder (Backspace)"),
            Action::Up,
        );
        ui.add_space(6.0);
        breadcrumb::show(ui, pool, nav.current(), &mut action);
        ui.add_space(10.0);
        let hint = if nav.current().is_empty() {
            tr("Put the whole drive back the way it was at an earlier time.")
        } else {
            tr("Put this folder back the way it was at an earlier time.")
        };
        if ui
            .button(format!("↺ {}", tr("Roll back…")))
            .on_hover_text(hint)
            .clicked()
        {
            action = Some(Action::History(HistoryRequest::Rollback(
                nav.current().to_string(),
            )));
        }
    });
    action
}
