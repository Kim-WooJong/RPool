//! Keyboard shortcuts of the explorer, active while no text field (or
//! other widget) holds the keyboard focus.

use super::action::Action;
use eframe::egui::{self, Key};

/// `vertical` is how many entries Up/Down move (a row of icons in the
/// icon view); Left/Right move by one there.
pub(crate) fn read(ui: &egui::Ui, vertical: usize, grid: bool) -> Option<Action> {
    if ui.memory(|memory| memory.focused().is_some()) {
        return None;
    }
    let step = vertical.max(1) as isize;
    ui.input(|input| {
        let alt = input.modifiers.alt;
        let pressed = |key| input.key_pressed(key);
        if alt && pressed(Key::ArrowLeft) {
            Some(Action::Back)
        } else if alt && pressed(Key::ArrowRight) {
            Some(Action::Forward)
        } else if pressed(Key::Backspace) || alt && pressed(Key::ArrowUp) {
            Some(Action::Up)
        } else if pressed(Key::Enter) {
            Some(Action::OpenSelected)
        } else if pressed(Key::ArrowDown) {
            Some(Action::Move(step))
        } else if pressed(Key::ArrowUp) {
            Some(Action::Move(-step))
        } else if grid && pressed(Key::ArrowRight) {
            Some(Action::Move(1))
        } else if grid && pressed(Key::ArrowLeft) {
            Some(Action::Move(-1))
        } else {
            None
        }
    })
}
