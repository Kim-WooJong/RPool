//! Step 5: clean up after a completed migration (`pool migrate retire` /
//! `restore`): the dry-run summary per account, "Move to cleanup
//! quarantine", the quarantine list with its countdown, "Restore" and,
//! after the grace period, "Delete permanently". A refusal of the
//! mass-delete guard needs an explicit second confirmation with the numbers.
use super::cleanup_state::CleanupAction;
use super::state::{short_id, Step};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::migration::retire::model::{
    AccountBytes, FossilState, FossilView, ItemKind, KeepReason, RetireReport, MAX_GRACE_DAYS,
};
use crate::presentation::format_bytes;
use eframe::egui;
use std::path::PathBuf;

pub(crate) fn keep_reason(reason: KeepReason) -> &'static str {
    match reason {
        KeepReason::DriveArchive => tr("drive revision (never cleaned up here)"),
        KeepReason::Lost => tr("not replaced; the original is all there is"),
        KeepReason::ReplacementUnverified => tr("replacement did not pass re-verification"),
        KeepReason::OriginalChanged => tr("original changed after the migration"),
        KeepReason::OriginalUnreadable => tr("original could not be read"),
        KeepReason::ObjectOutsideArchive => tr("an object is outside its archive folder"),
        KeepReason::Referenced => tr("still referenced"),
        KeepReason::ReferencesUncertain => tr("some references could not be read"),
        KeepReason::Unreachable => tr("an account could not be listed"),
        KeepReason::VerifiedCopy => tr("verified copy"),
        KeepReason::UnexpectedId => tr("unexpected id"),
    }
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Original => tr("Replaced original"),
        ItemKind::Orphan => tr("Partial copy"),
    }
}

/// "3d 4h left", "5h left", "under 1h left" or "ready to delete".
pub(crate) fn countdown(now: u64, due: u64) -> String {
    let left = due.saturating_sub(now);
    match left {
        0 => tr("ready to delete").into(),
        1..=3_599 => tr("under 1h left").into(),
        3_600..=86_399 => trf("{h}h left", &[("h", &(left / 3_600))]),
        _ => trf(
            "{d}d {h}h left",
            &[("d", &(left / 86_400)), ("h", &(left % 86_400 / 3_600))],
        ),
    }
}

fn state_text(view: &FossilView, now: u64) -> String {
    match view.state {
        FossilState::Waiting | FossilState::Due => countdown(now, view.due_unix),
        FossilState::Deleting => trf("deleting ({n} objects left)", &[("n", &view.remaining)]),
        FossilState::Purged => tr("deleted").into(),
    }
}

/// Workspaces of this pool's running drive sessions.
fn running_workspaces(state: &GuiState) -> Vec<PathBuf> {
    crate::gui::screens::storage::mount::sessions::mounted_sessions(state)
        .into_iter()
        .filter(|s| s.pool == state.migration.pool)
        .map(|s| s.workspace)
        .collect()
}

fn accounts_table(ui: &mut egui::Ui, id: &str, rows: &[AccountBytes]) {
    egui::Grid::new(id)
        .striped(true)
        .num_columns(3)
        .show(ui, |ui| {
            for head in [tr("Account"), tr("Objects"), tr("Space freed")] {
                ui.strong(head);
            }
            ui.end_row();
            for row in rows {
                ui.label(&row.root);
                ui.label(row.objects.to_string());
                ui.label(format_bytes(row.bytes));
                ui.end_row();
            }
        });
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let Some(id) = state.migration.active_id.clone() else {
        state.migration.reset_to_plan();
        return;
    };
    let complete = state
        .migration
        .active_status()
        .is_some_and(|s| s.complete && !s.abandoned);
    let workspaces = state
        .migration
        .cleanup
        .workspaces(&running_workspaces(state));
    let idle = !task.is_running();
    let rclone = state.settings.rclone.clone();
    let pool = state.migration.pool.clone();
    let form = &mut state.migration.cleanup;
    if complete && form.report_id.as_deref() != Some(id.as_str()) && !form.loading() && idle {
        form.start_report(&rclone, &pool, &id, workspaces.clone());
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(trf("Clean up migration {id}", &[("id", &short_id(&id))])).strong(),
        );
        if form.loading() {
            ui.spinner();
        }
        if ui
            .add_enabled(
                complete && idle && !form.loading(),
                egui::Button::new(tr("Check again")),
            )
            .clicked()
        {
            form.start_report(&rclone, &pool, &id, workspaces.clone());
        }
        if ui.button(tr("Back")).clicked() {
            state.migration.step = Step::Run;
        }
    });
    let form = &mut state.migration.cleanup;
    theme::hint(ui, tr("Frees the space of originals that a verified replacement superseded and of partial copies from interrupted attempts. \"Move to cleanup quarantine\" only records them: they stay readable and can be restored. After the grace period \"Delete permanently\" deletes them, after checking again that nothing refers to them. Anything still referenced, drive revisions and lost files are kept."));
    if !complete {
        theme::hint(
            ui,
            tr("Clean up is available once the migration is complete."),
        );
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(tr("Grace period (days)"));
        ui.add(egui::DragValue::new(&mut form.grace_days).range(0..=MAX_GRACE_DAYS));
        ui.checkbox(&mut form.include_removed, tr("Also delete on removed accounts"))
            .on_hover_text(tr("Originals' objects on accounts that left the pool are deleted too, when those accounts are still connected."));
        ui.checkbox(&mut form.full_verify, tr("Read replacements back in full"))
            .on_hover_text(tr("Slower: every shard of each replacement is downloaded and checked before its original may go."));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(tr("Drive workspace (optional)"))
            .on_hover_text(tr("A local drive workspace of this pool whose metadata should also count as references. Running drives of this pool are included automatically."));
        ui.add(egui::TextEdit::singleline(&mut form.workspace).desired_width(260.0));
    });
    if let Some(error) = &form.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    let Some(report) = form.report_for(&id).cloned() else {
        theme::hint(ui, tr("Checking what can be cleaned up…"));
        return;
    };
    summary(ui, &report);
    let mut request: Option<CleanupAction> = None;
    ui.separator();
    ui.strong(tr("Can be cleaned up"));
    if report.candidates.is_empty() {
        theme::hint(ui, tr("Nothing new to clean up."));
    } else {
        accounts_table(ui, "migration-cleanup-accounts", &report.candidate_accounts);
    }
    if theme::primary_button(
        ui,
        idle && !report.candidates.is_empty(),
        tr("Move to cleanup quarantine"),
    )
    .clicked()
    {
        request = Some(CleanupAction::Quarantine);
    }
    ui.separator();
    ui.strong(tr("Cleanup quarantine"));
    let now = crate::utils::now_unix();
    let due = report.quarantine.iter().any(|v| {
        matches!(v.state, FossilState::Due | FossilState::Deleting) && v.blocked.is_none()
    });
    if report.quarantine.is_empty() {
        theme::hint(ui, tr("The quarantine is empty."));
    } else {
        if let Some(action) = quarantine_table(ui, &report, now, idle) {
            request = Some(action);
        }
    }
    ui.horizontal_wrapped(|ui| {
        let restorable = report
            .quarantine
            .iter()
            .any(|v| matches!(v.state, FossilState::Waiting | FossilState::Due));
        if ui
            .add_enabled(idle && restorable, egui::Button::new(tr("Restore all")))
            .clicked()
        {
            request = Some(CleanupAction::Restore(vec![]));
        }
        if theme::danger_button(ui, idle && due, tr("Delete permanently"))
            .on_hover_text(tr("Deletes the quarantined items whose grace period has ended, after a fresh check. Cannot be undone."))
            .clicked()
        {
            request = Some(CleanupAction::Delete);
        }
    });
    kept_list(ui, &report);
    let form = &mut state.migration.cleanup;
    if let Some(action) = form.confirm_force.clone() {
        if let Some(force) = force_confirmation(ui, &report, &action) {
            if force {
                let args = form.args(&action, &pool, &id, &workspaces, true);
                if let Err(error) = state.migration.start_cleanup(task, &rclone, action, args) {
                    state.migration.error = Some(error);
                }
            } else {
                state.migration.cleanup.confirm_force = None;
            }
            return;
        }
    }
    if let Some(action) = request {
        if let Some(args) = state
            .migration
            .cleanup
            .request(action.clone(), &pool, &id, &workspaces)
        {
            if let Err(error) = state.migration.start_cleanup(task, &rclone, action, args) {
                state.migration.error = Some(error);
            }
        }
    }
}

fn summary(ui: &mut egui::Ui, report: &RetireReport) {
    let candidate_bytes: u64 = report.candidates.iter().map(|i| i.bytes()).sum();
    let quarantine_bytes: u64 = report.quarantine.iter().map(|v| v.item.bytes()).sum();
    ui.horizontal_wrapped(|ui| {
        status_badge(
            ui,
            &trf(
                "{n} can be cleaned up ({bytes})",
                &[
                    ("n", &report.candidates.len()),
                    ("bytes", &format_bytes(candidate_bytes)),
                ],
            ),
            if report.candidates.is_empty() {
                StatusTone::Neutral
            } else {
                StatusTone::Info
            },
        );
        status_badge(
            ui,
            &trf(
                "{n} in quarantine ({bytes})",
                &[
                    ("n", &report.quarantine.len()),
                    ("bytes", &format_bytes(quarantine_bytes)),
                ],
            ),
            StatusTone::Neutral,
        );
        status_badge(
            ui,
            &trf("{n} kept", &[("n", &report.kept.len())]),
            StatusTone::Neutral,
        );
        status_badge(
            ui,
            &trf("{n} deleted", &[("n", &report.purged)]),
            StatusTone::Success,
        );
    });
    if !report.uncertain.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            trf(
                "Some references could not be read, so nothing is deleted until they can: {what}",
                &[("what", &report.uncertain.join("; "))],
            ),
        );
    }
    if !report.left_on_removed.is_empty() {
        let (objects, bytes) = report
            .left_on_removed
            .iter()
            .fold((0, 0), |(o, b), a| (o + a.objects, b + a.bytes));
        theme::hint(
            ui,
            &trf(
                "{n} objects ({bytes}) stay on accounts that left the pool.",
                &[("n", &objects), ("bytes", &format_bytes(bytes))],
            ),
        );
    }
}

/// The quarantine list; returns a per-row restore request.
fn quarantine_table(
    ui: &mut egui::Ui,
    report: &RetireReport,
    now: u64,
    idle: bool,
) -> Option<CleanupAction> {
    let mut action = None;
    let height = theme::list_height(ui.ctx().content_rect().height());
    let size = egui::vec2(ui.available_width(), height);
    theme::fixed_pane_wide(ui, "migration-cleanup-quarantine", size, |ui| {
        egui::Grid::new("migration-cleanup-quarantine-grid")
            .striped(true)
            .num_columns(6)
            .show(ui, |ui| {
                for head in [
                    tr("Name"),
                    tr("Kind"),
                    tr("Size"),
                    tr("State"),
                    tr("Note"),
                    "",
                ] {
                    ui.strong(head);
                }
                ui.end_row();
                for view in &report.quarantine {
                    ui.label(&view.item.original_name)
                        .on_hover_text(&view.item.archive_id);
                    ui.label(kind_label(view.item.kind));
                    ui.label(format_bytes(view.item.bytes()));
                    ui.label(state_text(view, now));
                    match &view.blocked {
                        Some(why) => ui.label(trf("kept at deletion: {why}", &[("why", why)])),
                        None if view.restore_refused => {
                            ui.label(tr("restore came after deletion started"))
                        }
                        None => ui.label(""),
                    };
                    let restorable = matches!(view.state, FossilState::Waiting | FossilState::Due);
                    if ui
                        .add_enabled(idle && restorable, egui::Button::new(tr("Restore")))
                        .clicked()
                    {
                        action = Some(CleanupAction::Restore(vec![view.item.archive_id.clone()]));
                    }
                    ui.end_row();
                }
            });
    });
    action
}

fn kept_list(ui: &mut egui::Ui, report: &RetireReport) {
    if report.kept.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(trf("Kept ({n})", &[("n", &report.kept.len())]))
        .id_salt("migration-cleanup-kept")
        .show(ui, |ui| {
            let height = theme::list_height(ui.ctx().content_rect().height());
            let size = egui::vec2(ui.available_width(), height);
            theme::fixed_pane_wide(ui, "migration-cleanup-kept-pane", size, |ui| {
                egui::Grid::new("migration-cleanup-kept-grid")
                    .striped(true)
                    .num_columns(3)
                    .show(ui, |ui| {
                        for head in [tr("Name"), tr("Reason"), tr("Details")] {
                            ui.strong(head);
                        }
                        ui.end_row();
                        for kept in &report.kept {
                            ui.label(&kept.original_name)
                                .on_hover_text(&kept.archive_id);
                            ui.label(keep_reason(kept.reason));
                            ui.label(&kept.detail);
                            ui.end_row();
                        }
                    });
            });
        });
}

/// The guard's second confirmation: `Some(true)` confirmed, `Some(false)`
/// cancelled, `None` still open.
fn force_confirmation(
    ui: &mut egui::Ui,
    report: &RetireReport,
    action: &CleanupAction,
) -> Option<bool> {
    let g = &report.guard;
    let why = match action {
        CleanupAction::Quarantine => g.quarantine_refusal.as_deref(),
        CleanupAction::Delete => g.delete_refusal.as_deref(),
        CleanupAction::Restore(_) => None,
    }
    .unwrap_or_default();
    let mut answer = None;
    ui.separator();
    ui.colored_label(
        ui.visuals().warn_fg_color,
        trf(
            "The mass-delete guard stopped this step: {why}. The pool holds {objects} objects ({bytes}); the limit is {percent}% or {max} objects per run.",
            &[
                ("why", &why),
                ("objects", &g.pool_objects),
                ("bytes", &format_bytes(g.pool_bytes)),
                ("percent", &g.max_percent),
                ("max", &g.max_objects),
            ],
        ),
    );
    ui.horizontal_wrapped(|ui| {
        if theme::danger_button(ui, true, tr("I checked the numbers — continue anyway")).clicked()
        {
            answer = Some(true);
        }
        if ui.button(tr("Cancel")).clicked() {
            answer = Some(false);
        }
    });
    answer
}
