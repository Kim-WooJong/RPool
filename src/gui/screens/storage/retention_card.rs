//! "Trash & versions" card on the Pools page: how long deleted files stay
//! in the trash and how many / how long previous versions are kept, by
//! `rpool drive retention show|set` (0 means unlimited), and below it the
//! space a drive cleanup can reclaim (`cleanup_section`).

use crate::drive_history::model::Retention;
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::history::retention::{
    days_label, notes, validate, versions_label, MAX_DAYS, MAX_VERSIONS,
};
use crate::gui::screens::files::inventory::history::state::Change;
use crate::gui::screens::files::inventory::history::{args, changes};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;

pub(crate) fn pool_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let saved = [state.pools.name.trim(), state.pools.selected.as_str()]
        .into_iter()
        .find(|name| state.pool_definitions.contains_key(*name))
        .map(str::to_string);
    if std::mem::take(&mut state.inventory.drive.history.reveal_retention) {
        ui.scroll_to_cursor(Some(egui::Align::Min));
    }
    theme::card_section(
        ui,
        tr("Trash & versions"),
        Some(tr("Deleted files wait in the trash and earlier versions of changed files are kept, so they can be restored from Files › Library.")),
        |_| {},
        |ui| {
            let Some(pool) = saved else {
                theme::hint(ui, tr("Select and load a saved pool to see its trash and version settings."));
                return;
            };
            body(ui, state, task, &pool);
        },
    );
}

fn body(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner, pool: &str) {
    let rclone = state.settings.rclone.clone();
    let history = &mut state.inventory.drive.history;
    let running_here = history
        .running
        .as_ref()
        .is_some_and(|(p, change)| p == pool && matches!(change, Change::Retention(_)));
    let entry = history.pool(pool);
    if entry.retention.needs_load() {
        entry.retention.start(&rclone, args::retention_show(pool));
    }
    if entry.retention.poll() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    let saved = match &entry.retention.value {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::hint(ui, tr("Reading the settings…"));
            });
            return;
        }
        Some(Err(error)) => {
            ui.colored_label(
                theme::error_colors(ui.visuals().dark_mode).1,
                trf(
                    "The settings could not be read: {error}",
                    &[("error", error)],
                ),
            );
            if ui.button(tr("Retry")).clicked() {
                entry.retention.stale = true;
            }
            return;
        }
        Some(Ok(saved)) => *saved,
    };
    ui.weak(trf(
        "Saved: trash {trash} · versions per file {count} · version age {days}",
        &[
            ("trash", &days_label(saved.trash_days)),
            ("count", &versions_label(saved.keep_versions)),
            ("days", &days_label(saved.version_days)),
        ],
    ));
    let draft = entry.retention_draft.get_or_insert(saved);
    let editable = !running_here;
    ui.add_enabled_ui(editable, |ui| {
        egui::Grid::new(("retention-grid", pool))
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("Days in the trash"));
                ui.add(egui::DragValue::new(&mut draft.trash_days).range(0..=MAX_DAYS));
                ui.end_row();
                ui.label(tr("Versions to keep per file"));
                ui.add(egui::DragValue::new(&mut draft.keep_versions).range(0..=MAX_VERSIONS));
                ui.end_row();
                ui.label(tr("Days to keep versions"));
                ui.add(egui::DragValue::new(&mut draft.version_days).range(0..=MAX_DAYS));
                ui.end_row();
            });
    });
    theme::hint(ui, tr("0 means unlimited. A version is removed once it is past either limit; the current version is always kept."));
    let draft = *draft;
    for note in notes(draft) {
        theme::hint(ui, note);
    }
    let valid = validate(draft);
    if let Err(error) = valid {
        ui.colored_label(ui.visuals().error_fg_color, error.text());
    }
    let changed = draft != saved;
    let mut save = None;
    ui.horizontal_wrapped(|ui| {
        if theme::primary_button(
            ui,
            changed && valid.is_ok() && !task.is_running(),
            tr("Save"),
        )
        .clicked()
        {
            save = valid.ok();
        }
        if ui
            .add_enabled(changed && editable, egui::Button::new(tr("Undo changes")))
            .clicked()
        {
            entry.retention_draft = Some(saved);
        }
        if ui
            .add_enabled(
                editable && draft != Retention::default(),
                egui::Button::new(tr("Defaults")),
            )
            .on_hover_text(tr("30 days in the trash, 20 versions for 90 days."))
            .clicked()
        {
            entry.retention_draft = Some(Retention::default());
        }
        if running_here {
            ui.spinner();
        }
    });
    if let Some((success, text)) = &entry.retention_notice {
        badges_notice(ui, *success, text);
    }
    if let Some(values) = save {
        let args = args::retention_set(pool, values);
        changes::start(
            history,
            task,
            &rclone,
            pool,
            Change::Retention(values),
            args,
        );
    }
    super::cleanup_section::show(ui, history, task, &rclone, pool);
}

fn badges_notice(ui: &mut egui::Ui, success: bool, text: &str) {
    let dark = ui.visuals().dark_mode;
    let color = if success {
        theme::success_colors(dark).1
    } else {
        theme::error_colors(dark).1
    };
    ui.colored_label(color, text);
}
