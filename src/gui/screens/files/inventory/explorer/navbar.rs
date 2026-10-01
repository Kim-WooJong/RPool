//! Back / Forward / Up buttons and the breadcrumb path bar.

use super::action::Action;
use super::breadcrumb;
use super::nav::Nav;
use crate::gui::i18n::tr;
use eframe::egui;

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
    });
    action
}
