//! Right-click menu of an entry: Versions… for files, Open and Roll back…
//! for folders. Opening the menu also selects the entry.

use super::action::{Action, HistoryRequest};
use crate::gui::i18n::tr;
use crate::gui::screens::files::inventory::drive_state::DriveNode;
use eframe::egui;

/// Attaches the right-click menu to an entry's `response`: a right click selects
/// the entry, the menu items set `action`. Called by `list::row` and `icons::tile`.
pub(crate) fn menu(response: &egui::Response, node: &DriveNode, action: &mut Option<Action>) {
    if response.secondary_clicked() {
        *action = Some(Action::Select(node.path.clone()));
    }
    response.context_menu(|ui| {
        let path = node.path.clone();
        if node.is_dir {
            if ui.button(tr("Open")).clicked() {
                *action = Some(Action::Open(path.clone()));
                ui.close();
            }
            if ui.button(tr("Roll back this folder…")).clicked() {
                *action = Some(Action::History(HistoryRequest::Rollback(path)));
                ui.close();
            }
        } else if ui.button(tr("Versions…")).clicked() {
            *action = Some(Action::History(HistoryRequest::Versions(path)));
            ui.close();
        }
    });
}
