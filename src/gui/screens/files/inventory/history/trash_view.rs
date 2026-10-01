//! Library › Drive files › Trash: the deleted items of the pool's drive
//! with Restore, Restore to…, Delete permanently and Empty trash.

use super::state::{Change, HistoryForm, TrashDialog};
use super::trash_list::{self, RowAction};
use super::trash_rows::{self, TrashRow};
use super::{args, badges, changes, clock, format};
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

/// What the buttons under the list asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Row(RowAction),
    PurgeExpired,
    Empty,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    history: &mut HistoryForm,
    pool: &str,
    query: &str,
    task: &mut TaskRunner,
    rclone: &str,
) {
    let now = crate::utils::now_unix();
    let busy = task.is_running();
    let running_here = history.is_running(pool);
    let state = history.pool(pool);
    if let Some((success, text)) = &state.notice {
        badges::notice(ui, *success, text);
        ui.add_space(theme::SUBSECTION_GAP);
    }
    let entries = match &state.trash.value {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::hint(ui, tr("Reading the trash…"));
            });
            return;
        }
        Some(Err(error)) => {
            let error = trf("The trash could not be read: {error}", &[("error", error)]);
            ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
            if ui
                .add_enabled(!state.trash.is_loading(), egui::Button::new(tr("Retry")))
                .clicked()
            {
                state.trash.start(rclone, args::trash_list(pool));
            }
            return;
        }
        Some(Ok(entries)) => entries,
    };
    let listed: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    state.selection.retain(&listed);
    let (count, bytes) = trash_rows::all_totals(entries);
    let expired = entries
        .iter()
        .filter(|e| format::is_expired(now, e.expires_unix))
        .count();
    let rows = trash_rows::rows(entries, query, now, clock::offset(), &state.from_rollback);

    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("🗑 {}", tr("Trash")));
        ui.weak(if count == 1 {
            trf("1 item · {size}", &[("size", &format_bytes(bytes))])
        } else {
            trf(
                "{n} items · {size}",
                &[("n", &count), ("size", &format_bytes(bytes))],
            )
        });
        if state.trash.is_loading() {
            ui.spinner();
        }
    });
    if entries.is_empty() {
        ui.add_space(theme::SUBSECTION_GAP);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(tr("The trash is empty."));
            theme::hint(ui, tr("Deleted files and folders of this drive appear here until they expire, so you can restore them."));
        });
        return;
    }
    if rows.iter().any(|row| row.from_rollback) {
        badges::info(ui, tr("Items marked “from rollback” were created after the time a rollback went back to, so the rollback moved them here. Restore them to undo that part of the rollback."));
    }
    ui.add_space(theme::SUBSECTION_GAP);

    let mut request = None;
    let order: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let ids = state.selection.in_order(&order);
    let (picked, picked_bytes) = trash_rows::totals(&rows, &ids);
    let idle = !busy;
    ui.horizontal_wrapped(|ui| {
        if picked == 0 {
            ui.weak(tr("Nothing selected"));
        } else {
            ui.label(trf(
                "{n} selected · {size}",
                &[("n", &picked), ("size", &format_bytes(picked_bytes))],
            ));
            if ui.small_button(tr("Clear selection")).clicked() {
                state.selection.clear();
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let some = picked > 0 && idle;
        if theme::primary_button(ui, some, tr("Restore"))
            .on_hover_text(tr("Put the selected items back where they were."))
            .clicked()
        {
            request = Some(Request::Row(RowAction::Restore));
        }
        if ui
            .add_enabled(some, egui::Button::new(tr("Restore to…")))
            .on_hover_text(tr("Put the selected items into a folder you choose."))
            .clicked()
        {
            request = Some(Request::Row(RowAction::RestoreTo));
        }
        if theme::danger_button(ui, some, tr("Delete permanently…")).clicked() {
            request = Some(Request::Row(RowAction::Purge));
        }
        ui.add_space(12.0);
        if expired > 0
            && ui
                .add_enabled(
                    idle,
                    egui::Button::new(trf("Delete expired ({n})", &[("n", &expired)])),
                )
                .on_hover_text(tr(
                    "Permanently delete the items whose time in the trash is over.",
                ))
                .clicked()
        {
            request = Some(Request::PurgeExpired);
        }
        if ui
            .add_enabled(idle, egui::Button::new(tr("Empty trash…")))
            .clicked()
        {
            request = Some(Request::Empty);
        }
        if running_here {
            ui.spinner();
            ui.weak(tr("Working…"));
        }
    });
    let taken = rows
        .iter()
        .filter(|row| row.path_taken && ids.contains(&row.id))
        .count();
    if taken > 0 {
        theme::hint(
            ui,
            &trf(
                "Selected items with a file of the same name at their old place: {n}. Restore keeps both (the restored item gets a new name); Restore to… puts them into another folder.",
                &[("n", &taken)],
            ),
        );
    }

    ui.add_space(theme::SUBSECTION_GAP);
    let height = (ui.available_height() - 2.0 * trash_list::ROW - 24.0).max(3.0 * trash_list::ROW);
    theme::card(ui).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if rows.is_empty() {
            ui.label(egui::RichText::new(tr("No names match the search.")).weak());
            return;
        }
        if let Some(row) = trash_list::show(ui, &rows, &mut state.selection, height) {
            request = Some(Request::Row(row));
        }
    });

    if let Some(request) = request {
        apply(
            history,
            task,
            rclone,
            pool,
            request,
            &rows,
            ids,
            (count, bytes),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    history: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
    pool: &str,
    request: Request,
    rows: &[TrashRow],
    ids: Vec<String>,
    all: (usize, u64),
) {
    match request {
        Request::Row(RowAction::Restore) if !ids.is_empty() => {
            let count = ids.len();
            let args = args::trash_restore(pool, &ids, None);
            changes::start(history, task, rclone, pool, Change::Restore { count }, args);
        }
        Request::Row(RowAction::RestoreTo) if !ids.is_empty() => {
            history.dialog = Some(TrashDialog::RestoreTo {
                ids,
                folder: String::new(),
                filter: String::new(),
            });
        }
        Request::Row(RowAction::Purge) if !ids.is_empty() => {
            let (count, bytes) = trash_rows::totals(rows, &ids);
            history.dialog = Some(TrashDialog::Purge { ids, count, bytes });
        }
        Request::PurgeExpired => {
            let args = args::trash_purge_expired(pool);
            changes::start(history, task, rclone, pool, Change::PurgeExpired, args);
        }
        Request::Empty => {
            let (count, bytes) = all;
            history.dialog = Some(TrashDialog::Empty { count, bytes });
        }
        Request::Row(_) => {}
    }
}
