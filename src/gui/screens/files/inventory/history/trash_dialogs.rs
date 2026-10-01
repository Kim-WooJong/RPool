//! Confirmation dialogs of the trash: Delete permanently, Empty trash, and
//! Restore to… with a folder picked from the drive tree.

use super::state::{Change, HistoryForm, TrashDialog};
use super::{args, badges, changes, dialog, paths};
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

/// Folders listed in the picker at most (narrow the search for more).
const MAX_FOLDERS: usize = 300;

enum Outcome {
    Keep,
    Close,
    Run(Change, Vec<std::ffi::OsString>),
}

pub(crate) fn show(
    ctx: &egui::Context,
    history: &mut HistoryForm,
    pool: &str,
    tree: Option<&DriveTree>,
    task: &mut TaskRunner,
    rclone: &str,
) {
    let idle = !task.is_running();
    let Some(open) = history.dialog.as_mut() else {
        return;
    };
    let outcome = match open {
        TrashDialog::Purge { ids, count, bytes } => {
            let (outcome, close) = dialog::modal(
                ctx,
                "purge",
                tr("Delete permanently?"),
                440.0,
                |ui| {
                    badges::warning(
                    ui,
                    &trf(
                        "{n} items ({size}) will be deleted permanently. They cannot be restored afterwards.",
                        &[("n", count), ("size", &format_bytes(*bytes))],
                    ),
                );
                    buttons(ui, idle, tr("Delete permanently"), true)
                },
            );
            match outcome {
                Some(true) => Outcome::Run(
                    Change::Purge { count: *count },
                    args::trash_purge(pool, ids),
                ),
                Some(false) => Outcome::Close,
                None if close => Outcome::Close,
                None => Outcome::Keep,
            }
        }
        TrashDialog::Empty { count, bytes } => {
            let (outcome, close) = dialog::modal(
                ctx,
                "empty",
                tr("Empty the trash?"),
                440.0,
                |ui| {
                    badges::warning(
                    ui,
                    &trf(
                        "All {n} items in the trash ({size}) will be deleted permanently. They cannot be restored afterwards.",
                        &[("n", count), ("size", &format_bytes(*bytes))],
                    ),
                );
                    buttons(ui, idle, tr("Empty trash"), true)
                },
            );
            match outcome {
                Some(true) => Outcome::Run(Change::Empty, args::trash_empty(pool)),
                Some(false) => Outcome::Close,
                None if close => Outcome::Close,
                None => Outcome::Keep,
            }
        }
        TrashDialog::RestoreTo {
            ids,
            folder,
            filter,
        } => {
            let (outcome, close) =
                dialog::modal(ctx, "restore-to", tr("Restore to a folder"), 480.0, |ui| {
                    ui.label(if ids.len() == 1 {
                        tr("Choose the folder to put the item into.").to_string()
                    } else {
                        trf(
                            "Choose the folder to put the {n} items into.",
                            &[("n", &ids.len())],
                        )
                    });
                    folder_picker(ui, tree, folder, filter);
                    ui.add_space(theme::SUBSECTION_GAP);
                    buttons(ui, idle, tr("Restore here"), false)
                });
            match outcome {
                Some(true) => Outcome::Run(
                    Change::Restore { count: ids.len() },
                    args::trash_restore(pool, ids, Some(&paths::to_drive(folder))),
                ),
                Some(false) => Outcome::Close,
                None if close => Outcome::Close,
                None => Outcome::Keep,
            }
        }
    };
    match outcome {
        Outcome::Keep => {}
        Outcome::Close => history.dialog = None,
        Outcome::Run(change, args) => {
            history.dialog = None;
            changes::start(history, task, rclone, pool, change, args);
        }
    }
}

/// `Some(true)`: confirmed, `Some(false)`: cancelled.
fn buttons(ui: &mut egui::Ui, idle: bool, confirm: &str, danger: bool) -> Option<bool> {
    let mut outcome = None;
    ui.add_space(theme::SUBSECTION_GAP);
    ui.horizontal_wrapped(|ui| {
        let clicked = if danger {
            theme::danger_button(ui, idle, confirm)
        } else {
            theme::primary_button(ui, idle, confirm)
        }
        .clicked();
        if clicked {
            outcome = Some(true);
        }
        if ui.button(tr("Cancel")).clicked() {
            outcome = Some(false);
        }
    });
    if !idle {
        theme::hint(
            ui,
            tr("Another operation is running; wait for it to finish."),
        );
    }
    outcome
}

/// Sorted folder paths of the tree (explorer paths), the root first.
pub(crate) fn folders(tree: Option<&DriveTree>, filter: &str) -> Vec<String> {
    let filter = filter.trim().to_lowercase();
    let mut out: Vec<String> = tree
        .into_iter()
        .flat_map(|tree| tree.nodes.iter())
        .filter(|node| node.is_dir && node.path.to_lowercase().contains(&filter))
        .map(|node| node.path.clone())
        .collect();
    out.sort_by_key(|path| path.to_lowercase());
    if filter.is_empty() {
        out.insert(0, String::new());
    }
    out
}

fn folder_picker(
    ui: &mut egui::Ui,
    tree: Option<&DriveTree>,
    folder: &mut String,
    filter: &mut String,
) {
    ui.add(
        egui::TextEdit::singleline(filter)
            .hint_text(tr("Search folders…"))
            .desired_width(f32::INFINITY),
    );
    let list = folders(tree, filter);
    theme::card(ui).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical()
            .id_salt("restore-to-folders")
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if list.is_empty() {
                    ui.weak(tr("No folders match."));
                }
                for path in list.iter().take(MAX_FOLDERS) {
                    let label = if path.is_empty() {
                        format!("📁 {}", tr("(drive root)"))
                    } else {
                        format!("📁 {path}")
                    };
                    if ui.selectable_label(folder == path, label).clicked() {
                        *folder = path.clone();
                    }
                }
                if list.len() > MAX_FOLDERS {
                    ui.weak(trf(
                        "{n} more folders: narrow the search.",
                        &[("n", &(list.len() - MAX_FOLDERS))],
                    ));
                }
            });
    });
    ui.label(trf(
        "Restore into: {folder}",
        &[("folder", &paths::to_drive(folder))],
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_choices_come_from_the_tree() {
        let tree = crate::gui::screens::files::inventory::explorer::sample::tree();
        let all = folders(Some(&tree), "");
        assert_eq!(all[0], "", "the drive root comes first");
        assert!(all.contains(&"Photos/2024/trip".to_string()));
        assert!(all.iter().all(|p| p.is_empty() || tree.folder(p).is_some()));
        let found = folders(Some(&tree), "TRIP");
        assert_eq!(found, ["Photos/2024/trip"]);
        assert_eq!(folders(None, ""), [""]);
    }
}
