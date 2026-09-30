use crate::gui::i18n::{tr, trf};
use eframe::egui;

#[derive(Debug, Default)]
pub(super) struct PoolPicker {
    open: bool,
    draft: Vec<String>,
}

#[derive(Default)]
pub(super) struct PickerAction {
    pub(super) refresh: bool,
    pub(super) setup: bool,
}

impl PoolPicker {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self, selected: &[String]) {
        self.draft = selected.to_vec();
        self.open = true;
    }

    fn set_selected(&mut self, target: &str, selected: bool) {
        if selected {
            if !self.draft.iter().any(|item| item == target) {
                self.draft.push(target.to_string());
            }
        } else {
            self.draft.retain(|item| item != target);
        }
    }

    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        selected: &mut Vec<String>,
        discovered: &[String],
    ) -> PickerAction {
        let mut action = PickerAction::default();
        if !self.open {
            return action;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(tr("Choose encrypted providers"))
            .id(egui::Id::new("pool-provider-picker"))
            .open(&mut open)
            .default_width(540.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(tr("Add or remove destinations for this pool draft."));
                ui.small(tr("Removing a selection does not delete the provider or its cloud data."));
                ui.horizontal(|ui| {
                    if ui.button(tr("Refresh providers")).clicked() {
                        action.refresh = true;
                    }
                    if ui.button(tr("Set up provider…")).clicked() {
                        action.setup = true;
                    }
                });
                if discovered.is_empty() {
                    ui.label(tr("No encrypted providers discovered. Set up encryption in Providers, then refresh."));
                }
                egui::ScrollArea::both()
                    .id_salt("pool-provider-picker-list")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for target in choices(discovered) {
                            let mut checked = self.draft.contains(&target);
                            if ui.checkbox(&mut checked, &target).changed() {
                                self.set_selected(&target, checked);
                            }
                        }
                        ui.separator();
                        ui.label(trf("Selected destinations ({n})", &[("n", &self.draft.len())]));
                        // Includes custom paths and unavailable remotes; never silently
                        // replaces saved paths with today's provider defaults.
                        for target in self.draft.clone() {
                            ui.horizontal(|ui| {
                                ui.monospace(&target);
                                if ui.button(tr("Remove")).clicked() {
                                    self.set_selected(&target, false);
                                }
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    apply = ui.button(tr("Apply selection")).clicked();
                    cancel = ui.button(tr("Cancel")).clicked();
                });
                ui.small(tr("Apply updates this draft only; no cloud data is changed."));
            });
        if apply {
            *selected = self.draft.clone();
        }
        self.open = open && !apply && !cancel && !action.setup;
        action
    }
}

// Pool destinations use the discovered crypt root, not a configured upload folder.
fn choices(discovered: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    for remote in discovered {
        if !targets.contains(remote) {
            targets.push(remote.clone());
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_apply_and_cancel_buttons_commit_or_discard_changes() {
        for (button, commits) in [("Apply selection", true), ("Cancel", false)] {
            let ctx = egui::Context::default();
            let original = vec!["custom:saved-path".into(), "unavailable:old".into()];
            let mut selected = original.clone();
            let mut picker = PoolPicker::default();
            picker.open(&selected);
            picker.set_selected("custom:saved-path", false);
            picker.set_selected("new:root", true);
            let expected = picker.draft.clone();
            let mut frame = |events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, 800.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        picker.show(ui.ctx(), &mut selected, &[]);
                    },
                );
                let shapes = std::mem::take(&mut output.shapes);
                output.drop_without_applying_deltas();
                shapes
            };
            frame(Vec::new());
            let output = frame(Vec::new());
            // Locate the rendered label instead of relying on theme/layout coordinates.
            let position = output
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == button => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("picker button must be rendered");
            frame(vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            frame(vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!(!picker.is_open(), "{button} must close the picker");
            assert_eq!(selected, if commits { expected } else { original });
        }
    }

    #[test]
    fn provider_choices_use_crypt_roots_without_appending_default_paths() {
        let remotes = ["one:", "two:", "one:", "one:existing"].map(String::from);
        assert_eq!(choices(&remotes), ["one:", "two:", "one:existing"]);
    }

    #[test]
    fn cancelled_draft_preserves_custom_and_unavailable_destinations() {
        let original = vec!["custom:saved-path".into(), "unavailable:old".into()];
        let mut picker = PoolPicker::default();
        picker.open(&original);
        picker.set_selected("custom:saved-path", false);
        picker.set_selected("new:root", true);
        picker.set_selected("new:root", true);
        assert_eq!(picker.draft, ["unavailable:old", "new:root"]);
        assert_eq!(original, ["custom:saved-path", "unavailable:old"]);
        // Cancel/close never touches caller selection; reopening discards draft.
        picker.open(&original);
        assert_eq!(picker.draft, original);
        picker.set_selected("unavailable:old", false);
        assert_eq!(picker.draft, ["custom:saved-path"]);
    }
}
