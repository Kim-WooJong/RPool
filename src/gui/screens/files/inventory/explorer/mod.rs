//! Library › Drive files as a read-only file explorer: one folder at a
//! time with a path bar, history, sortable columns, a List or Icons view
//! and name search in the folder or the whole drive. Names only: nothing
//! is opened, downloaded or changed.

mod action;
mod breadcrumb;
mod columns;
mod context;
mod details;
mod icons;
mod keys;
mod list;
mod nav;
mod navbar;
pub(crate) mod paint;
#[cfg(any(test, debug_assertions))]
pub(crate) mod sample;
mod scroll;
mod search;
mod sort;
mod state;
mod summary;

pub(crate) use action::HistoryRequest;
pub(crate) use state::{ExplorerState, ViewMode};
pub(crate) use summary::{files_label, folders_label};

use super::drive_state::DriveTree;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use eframe::egui;
use summary::FolderSummary;

/// Room kept below the list for the details and summary lines.
const FOOTER: f32 = 64.0;

/// Draws the explorer; returns a request for the versions panel or the
/// rollback dialog when the user asked for one.
pub(crate) fn show(
    ui: &mut egui::Ui,
    pool: &str,
    tree: &DriveTree,
    state: &mut ExplorerState,
    query: &mut String,
) -> Option<HistoryRequest> {
    state.sync(tree);
    let folder = tree.folder(state.nav.current()).unwrap_or(None);
    let global = search::is_global(query, state.search_all);
    let mut rows = search::entries(tree, folder, query, state.search_all);
    state.sort.apply(tree, &mut rows);

    let mut action = navbar::show(ui, pool, &state.nav);
    ui.add_space(theme::SUBSECTION_GAP);
    let height = (ui.available_height() - FOOTER - 3.0 * list::ROW).max(100.0);
    let grid = state.view == ViewMode::Icons;
    theme::card(ui).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let vertical = if grid {
            icons::columns(ui.available_width())
        } else {
            1
        };
        if let Some(key) = keys::read(ui, vertical, grid) {
            action = Some(key);
        }
        if rows.is_empty() {
            let empty = if query.trim().is_empty() {
                tr("This folder is empty.")
            } else {
                tr("No names match the search.")
            };
            ui.label(egui::RichText::new(empty).weak());
            return;
        }
        let clicked = if grid {
            icons::show(ui, tree, &rows, state, height)
        } else {
            list::show(ui, tree, &rows, state, global, height)
        };
        if clicked.is_some() {
            action = clicked;
        }
    });

    let selected = state.selected.as_deref().and_then(|path| tree.find(path));
    let mut summary = if global {
        FolderSummary {
            folders: tree.dirs,
            files: tree.files,
            bytes: tree.bytes,
        }
    } else {
        FolderSummary::of(tree, folder)
    }
    .label();
    if !query.trim().is_empty() {
        let n = rows.len();
        let found = if n == 1 {
            trf("{n} match", &[("n", &n)])
        } else {
            trf("{n} matches", &[("n", &n)])
        };
        summary = format!("{found} · {summary}");
    }
    details::show(ui, tree, selected, &summary, global, &mut action);

    match action {
        Some(action::Action::History(request)) => return Some(request),
        Some(action) => {
            state.apply(action, tree, &rows, query);
            ui.ctx().request_repaint();
        }
        None => {}
    }
    None
}
