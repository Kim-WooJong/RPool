//! The line under the list: the selected entry, then the folder summary.

use super::action::{Action, HistoryRequest};
use super::summary::{icon, items_label, type_label};
use crate::gui::i18n::tr;
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) fn show(
    ui: &mut egui::Ui,
    tree: &DriveTree,
    selected: Option<usize>,
    summary: &str,
    global: bool,
    action: &mut Option<Action>,
) {
    ui.horizontal_wrapped(|ui| match selected {
        Some(id) => {
            let node = &tree.nodes[id];
            ui.label(format!("{} {}", icon(node.is_dir), node.path));
            let size = if node.is_dir {
                format!(
                    "{} · {}",
                    items_label(node.children.len()),
                    format_bytes(node.size)
                )
            } else {
                format_bytes(node.size)
            };
            ui.weak(format!(
                "· {size} · {}",
                type_label(&node.name, node.is_dir)
            ));
            if global && ui.link(tr("Show in folder")).clicked() {
                *action = Some(Action::Reveal(node.path.clone()));
            }
            if !node.is_dir
                && ui
                    .small_button(format!("🕘 {}", tr("Versions…")))
                    .on_hover_text(tr("Earlier versions of this file, to restore one."))
                    .clicked()
            {
                *action = Some(Action::History(HistoryRequest::Versions(node.path.clone())));
            }
        }
        None => {
            ui.weak(tr("Nothing selected"));
        }
    });
    ui.weak(summary);
}
