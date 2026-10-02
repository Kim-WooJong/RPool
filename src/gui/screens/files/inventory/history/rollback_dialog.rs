//! "Roll back…": pick a time, preview what changes (grouped), then apply
//! after a second confirmation. Rollback is reversible: changed files get a
//! new version with the old content, and files created after the time move
//! to the trash.

use super::rollback_summary::{Group, RollbackSummary};
use super::rollback_time::{resolve, TimePreset};
use super::state::{Change, HistoryForm, RollbackDialog};
use super::{args, badges, changes, clock, dialog};
use crate::drive_history::model::RollbackPlan;
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

/// Paths listed per group at most (the rest is counted).
const MAX_LISTED: usize = 200;

/// What the dialog body asked for this frame.
enum Step {
    /// Nothing to do; stay open.
    Keep,
    /// Close the dialog (ignored while the rollback is applying).
    Close,
    /// Start a preview query for this unix time.
    Preview(u64),
    /// Apply the rollback to this unix time, confirmed for this previewed summary.
    Apply(u64, RollbackSummary),
}

/// Draws the open "Roll back…" modal of `history.rollback`, if any: starts
/// previews as read-only queries and the confirmed rollback through
/// `changes::start`. Shows a failed apply's notice inside the dialog.
/// Called by `history::dialogs` every frame.
pub(crate) fn show(
    ctx: &egui::Context,
    history: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
) {
    let idle = !task.is_running();
    let Some(pool) = history.rollback.as_ref().map(|open| open.pool.clone()) else {
        return;
    };
    // A failed apply: say so here, where the user is looking.
    let failure = history
        .get(&pool)
        .and_then(|state| state.notice.clone())
        .filter(|(success, _)| !success)
        .map(|(_, text)| text);
    let Some(open) = history.rollback.as_mut() else {
        return;
    };
    let applying = history.running.as_ref().is_some_and(|(pool, change)| {
        *pool == open.pool && matches!(change, Change::Rollback { .. })
    });
    let title = if open.scope == "/" {
        tr("Roll back the whole drive").to_string()
    } else {
        trf("Roll back “{folder}”", &[("folder", &open.scope)])
    };
    let (step, close) = dialog::modal(ctx, "rollback", &title, 560.0, |ui| {
        if let Some(text) = &failure {
            badges::notice(ui, false, text);
            ui.add_space(theme::SUBSECTION_GAP);
        }
        body(ui, open, idle, applying)
    });
    let step = if close && !applying {
        Step::Close
    } else {
        step
    };
    match step {
        Step::Keep => {}
        Step::Close => history.rollback = None,
        Step::Preview(at) => {
            let args = args::rollback(&open.pool, &open.scope, at, false);
            open.confirming = false;
            open.preview.start(rclone, args);
        }
        Step::Apply(at, summary) => {
            let args = args::rollback(&pool, &open.scope, at, true);
            let change = Change::Rollback {
                changes: summary.changes(),
                trashed: summary.trashed_paths(),
            };
            changes::start(history, task, rclone, &pool, change, args);
        }
    }
}

/// Dialog body: time presets or custom time, Preview, the grouped preview and
/// the two-step Apply. Changing the time discards the old preview.
fn body(ui: &mut egui::Ui, open: &mut RollbackDialog, idle: bool, applying: bool) -> Step {
    let now = crate::utils::now_unix();
    let offset = clock::offset();
    badges::info(
        ui,
        tr("Rollback puts files back the way they were at the chosen time. It can be undone: changed files get a new version with the old content, and files created after that time move to the trash."),
    );
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(tr("Go back to"));
    let before = (open.preset, open.custom.clone());
    ui.add_enabled_ui(!applying, |ui| {
        ui.horizontal_wrapped(|ui| {
            for preset in TimePreset::ALL {
                ui.selectable_value(&mut open.preset, preset, preset.label());
            }
        });
        if open.preset == TimePreset::Custom {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut open.custom)
                        .hint_text("YYYY-MM-DD HH:MM")
                        .desired_width(160.0),
                );
                ui.weak(crate::utils::offset_label(offset));
            });
        }
    });
    if before != (open.preset, open.custom.clone()) {
        // Another time: the preview no longer matches it.
        open.preview = Default::default();
        open.confirming = false;
    }
    let at = resolve(open.preset, &open.custom, now, offset);
    match at {
        Ok(at) => theme::hint(
            ui,
            &trf(
                "Back to {time} ({zone}).",
                &[
                    ("time", &clock::format_local(at, offset)),
                    ("zone", &crate::utils::offset_label(offset)),
                ],
            ),
        ),
        Err(error) => {
            ui.colored_label(ui.visuals().error_fg_color, error.text());
        }
    }
    ui.add_space(theme::SUBSECTION_GAP);

    let mut step = Step::Keep;
    let loading = open.preview.is_loading();
    ui.horizontal_wrapped(|ui| {
        let label = if open.preview.value.is_some() {
            tr("Preview again")
        } else {
            tr("Preview changes")
        };
        if theme::primary_button(ui, at.is_ok() && !loading && !applying, label).clicked() {
            if let Ok(at) = at {
                step = Step::Preview(at);
            }
        }
        if ui
            .add_enabled(!applying, egui::Button::new(tr("Close")))
            .clicked()
        {
            step = Step::Close;
        }
        if loading {
            ui.spinner();
            ui.weak(tr("Working out the changes…"));
        }
    });

    let plan = match &open.preview.value {
        Some(Ok(plan)) => plan,
        Some(Err(error)) => {
            ui.colored_label(
                theme::error_colors(ui.visuals().dark_mode).1,
                trf("The preview failed: {error}", &[("error", error)]),
            );
            return step;
        }
        None => return step,
    };
    let summary = RollbackSummary::of(plan);
    ui.separator();
    preview(ui, plan, &summary, offset);
    if summary.changes() == 0 {
        return step;
    }
    ui.add_space(theme::SUBSECTION_GAP);
    if applying {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tr("Rolling back…"));
        });
    } else if open.confirming {
        badges::warning(
            ui,
            &trf(
                "Apply {n} changes? Files are put back as they were at {time}; you can undo this by restoring versions or items from the trash.",
                &[
                    ("n", &summary.changes()),
                    ("time", &clock::format_local(plan.at_unix, offset)),
                ],
            ),
        );
        ui.horizontal_wrapped(|ui| {
            if theme::danger_button(ui, idle, tr("Yes, apply rollback")).clicked() {
                step = Step::Apply(plan.at_unix, summary.clone());
            }
            if ui.button(tr("Back")).clicked() {
                open.confirming = false;
            }
        });
    } else if theme::danger_button(ui, idle && !loading, tr("Apply rollback…")).clicked() {
        open.confirming = true;
    }
    if !idle && !applying {
        theme::hint(
            ui,
            tr("Another operation is running; wait for it to finish."),
        );
    }
    step
}

/// The grouped summary and the expandable lists.
fn preview(ui: &mut egui::Ui, plan: &RollbackPlan, summary: &RollbackSummary, offset: i64) {
    if summary.changes() == 0 && summary.skipped.is_empty() {
        ui.label(trf(
            "Nothing changed here since {time}.",
            &[("time", &clock::format_local(plan.at_unix, offset))],
        ));
        return;
    }
    ui.strong(trf("{n} changes", &[("n", &summary.changes())]));
    egui::ScrollArea::vertical()
        .id_salt("rollback-preview")
        .max_height((ui.available_height() - 120.0).clamp(120.0, 320.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            group(
                ui,
                "reverted",
                &summary.reverted,
                tr("Back to an older version ({n})"),
            );
            group(
                ui,
                "restored",
                &summary.restored,
                tr("Deleted, coming back ({n})"),
            );
            group(
                ui,
                "trashed",
                &summary.trashed,
                tr("Newer, moving to the trash ({n})"),
            );
            if !summary.skipped.is_empty() {
                let warn = theme::warning_colors(ui.visuals().dark_mode).1;
                egui::CollapsingHeader::new(
                    egui::RichText::new(trf(
                        "Cannot be rolled back ({n})",
                        &[("n", &summary.skipped.len())],
                    ))
                    .color(warn),
                )
                .id_salt("rollback-skipped")
                .show(ui, |ui| {
                    for (path, reason) in summary.skipped.iter().take(MAX_LISTED) {
                        ui.add(egui::Label::new(path.as_str()).truncate());
                        ui.weak(format!("  {reason}"));
                    }
                    more(ui, summary.skipped.len());
                });
            }
        });
}

/// One collapsible preview group (at most `MAX_LISTED` paths); `title` holds
/// a `{n}` placeholder for the count. Empty groups are a plain weak line.
fn group(ui: &mut egui::Ui, id: &str, group: &Group, title: &'static str) {
    let n = group.changes.len();
    let heading = format!(
        "{} · {}",
        title.replace("{n}", &n.to_string()),
        format_bytes(group.bytes)
    );
    if n == 0 {
        ui.weak(heading);
        return;
    }
    egui::CollapsingHeader::new(heading)
        .id_salt(("rollback-group", id))
        .show(ui, |ui| {
            for change in group.changes.iter().take(MAX_LISTED) {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(change.path.as_str()).truncate());
                });
            }
            more(ui, n);
        });
}

/// "…and N more" when a list was cut at `MAX_LISTED`.
fn more(ui: &mut egui::Ui, n: usize) {
    if n > MAX_LISTED {
        ui.weak(trf("…and {n} more", &[("n", &(n - MAX_LISTED))]));
    }
}
