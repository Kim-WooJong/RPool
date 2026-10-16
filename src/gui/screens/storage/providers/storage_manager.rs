//! "Manage storage…" of a provider card: what RPool stores on one crypt
//! remote, grouped by owner (`provider storage list --json`, run as a query
//! off the UI thread), and the cleanup of groups no configured pool needs
//! (`provider storage clean`: preview as a query, "Mark for deletion" and
//! "Delete now" in the task runner). Only deletable groups can be ticked;
//! older generations nobody confirmed and other pools need an extra
//! confirmation. Every decision is the CLI's; this dialog only shows it.
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::history::clock;
use crate::gui::screens::files::inventory::history::query::Fetch;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use crate::provider::storage::model::{Ack, CleanReport, Group, GroupKind, StorageView};
use eframe::egui;
use std::collections::BTreeSet;
use std::ffi::OsString;

/// Task name of the cleanup runs started here.
pub(crate) const STORAGE_TASK: &str = "Clean up provider storage";

/// State of the "Manage storage" dialog (`ProviderForm::storage`).
#[derive(Debug, Default)]
pub(crate) struct Manager {
    /// Whether the dialog is shown.
    pub(crate) open: bool,
    /// Crypt remotes of the provider the dialog was opened for.
    crypts: Vec<String>,
    /// Crypt remote shown.
    crypt: String,
    /// The grouped view (`provider storage list`).
    view: Fetch<StorageView>,
    /// The preview of the selected groups (`provider storage clean --json`).
    preview: Fetch<CleanReport>,
    /// The choice `preview` was started for (`force` off).
    previewed: Option<Choice>,
    /// Ticked group ids.
    selected: BTreeSet<String>,
    /// Ticked groups the acknowledgements and `force` were given for.
    acked_for: Vec<String>,
    /// "No PC still uses these older generations" (`--include-unconfirmed-generations`).
    unconfirmed: bool,
    /// "These pools are not used any more" (`--include-foreign-pools`).
    foreign: bool,
    /// Pass the mass-delete guard (`--force`); sent only with the preview
    /// of exactly the current choice showing the guard.
    force: bool,
    /// "Delete now" asks once more.
    confirm_delete: bool,
    /// A cleanup run of this dialog is in the task runner; refresh when it ends.
    started: bool,
    /// Result of the last start, shown in the dialog.
    notice: Option<String>,
}

impl Manager {
    /// Opens the dialog for a provider's crypt remotes (the first one shown).
    pub(crate) fn open_for(crypts: &[String]) -> Self {
        Self {
            open: true,
            crypts: crypts.to_vec(),
            crypt: crypts.first().cloned().unwrap_or_default(),
            ..Self::default()
        }
    }
    /// Forgets the selection and results (another crypt remote was chosen).
    fn reset(&mut self) {
        self.view = Fetch::default();
        self.clear_preview();
        self.selected.clear();
        self.acked_for.clear();
        (self.unconfirmed, self.foreign) = (false, false);
        self.confirm_delete = false;
        self.notice = None;
    }
    /// Drops the preview and the `force` given for it.
    fn clear_preview(&mut self) {
        self.preview = Fetch::default();
        self.previewed = None;
        self.force = false;
    }
    /// Forgets the acknowledgements, `force`, the preview and a pending
    /// "Delete now" when the ticked groups are no longer those they were
    /// given for (ticked, unticked, or gone after a refresh).
    fn sync_selection(&mut self, ticked: &[String]) {
        if self.acked_for != ticked {
            self.acked_for = ticked.to_vec();
            (self.unconfirmed, self.foreign) = (false, false);
            self.confirm_delete = false;
            self.clear_preview();
        }
    }
    /// Drops a preview of another choice than `choice` (`force` off).
    fn drop_stale_preview(&mut self, choice: &Choice) {
        if self.previewed.as_ref().is_some_and(|p| p != choice) {
            self.clear_preview();
        }
    }
    /// Whether to send `--force`: ticked for the finished preview of exactly
    /// `choice` (`force` off), which showed the mass-delete guard.
    fn guard_passed(&self, choice: &Choice) -> bool {
        self.force
            && self.previewed.as_ref() == Some(choice)
            && matches!(&self.preview.value, Some(Ok(report)) if report.guard.is_some())
    }
}

/// `provider storage list --remote <crypt> --json`.
pub(crate) fn list_args(crypt: &str) -> Vec<OsString> {
    ["provider", "storage", "list", "--remote", crypt, "--json"]
        .iter()
        .map(OsString::from)
        .collect()
}

/// What a cleanup command should do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Choice {
    /// Group ids.
    pub(crate) groups: Vec<String>,
    /// `--include-unconfirmed-generations`.
    pub(crate) unconfirmed: bool,
    /// `--include-foreign-pools`.
    pub(crate) foreign: bool,
    /// `--force`.
    pub(crate) force: bool,
}

/// What a `provider storage clean` command of the dialog does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Run {
    /// JSON preview (writes nothing).
    Preview,
    /// "Mark for deletion": `--confirm --mark-only` (deletes nothing).
    Mark,
    /// "Delete now": `--confirm` (marks new data, deletes due data).
    Delete,
}

/// `provider storage clean …`: a JSON preview, or a confirmed run for the
/// task runner (`--force` only when deleting).
pub(crate) fn clean_args(crypt: &str, choice: &Choice, run: Run) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["provider", "storage", "clean", "--remote", crypt]
        .iter()
        .map(OsString::from)
        .collect();
    for group in &choice.groups {
        args.extend(["--group".into(), group.into()]);
    }
    if choice.unconfirmed {
        args.push("--include-unconfirmed-generations".into());
    }
    if choice.foreign {
        args.push("--include-foreign-pools".into());
    }
    match run {
        Run::Preview => args.push("--json".into()),
        Run::Mark => args.extend(["--confirm".into(), "--mark-only".into()]),
        Run::Delete => {
            args.push("--confirm".into());
            if choice.force {
                args.push("--force".into());
            }
        }
    }
    args
}

/// Translated owner category.
fn kind_label(kind: GroupKind) -> &'static str {
    match kind {
        GroupKind::InUse => tr("In use"),
        GroupKind::OldGeneration => tr("Older drive generation"),
        GroupKind::ForeignPool => tr("Other pool"),
        GroupKind::MigrationLeftovers => tr("Migration leftovers"),
        GroupKind::UnknownPutArchive => tr("Unknown upload"),
        GroupKind::IncompleteUpload => tr("Incomplete upload"),
        GroupKind::SpeedTestLeftovers => tr("Speed-test leftovers"),
        GroupKind::Journal => tr("Cleanup journal"),
        GroupKind::NotRpool => tr("Not RPool data"),
    }
}

/// Translated status of a group.
fn status_label(group: &Group) -> &'static str {
    match (group.deletable, group.needs.is_empty()) {
        (false, _) => tr("Kept"),
        (true, false) => tr("Deletable after confirming"),
        (true, true) => tr("Deletable"),
    }
}

/// Pending marks of a group, or empty.
fn pending_label(group: &Group, now: u64) -> String {
    let pending = &group.pending;
    if pending.marked == 0 {
        return String::new();
    }
    match pending.first_due_unix {
        Some(due) if due <= now => trf("{n} marked · due now", &[("n", &pending.marked)]),
        Some(due) => trf(
            "{n} marked · due {date}",
            &[
                ("n", &pending.marked),
                ("date", &clock::format_local(due, clock::offset())),
            ],
        ),
        None => trf("{n} marked", &[("n", &pending.marked)]),
    }
}

/// The ticked groups that still exist and are deletable.
fn ticked<'a>(view: &'a StorageView, selected: &BTreeSet<String>) -> Vec<&'a Group> {
    view.groups
        .iter()
        .filter(|g| g.deletable && selected.contains(&g.id))
        .collect()
}

/// Draws the dialog; starts queries and cleanup runs.
pub(crate) fn show(
    ctx: &egui::Context,
    manager: &mut Manager,
    rclone: &str,
    task: &mut TaskRunner,
) {
    if !manager.open {
        return;
    }
    if manager.started && !task.is_running() {
        manager.started = false;
        manager.view.stale = true;
        manager.clear_preview();
    }
    if !manager.crypt.is_empty() && manager.view.needs_load() {
        manager.view.start(rclone, list_args(&manager.crypt));
    }
    if manager.view.poll() | manager.preview.poll() {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    let mut open = true;
    egui::Window::new(tr("Manage storage"))
        .id(egui::Id::new("provider-storage-manager"))
        .open(&mut open)
        .resizable(true)
        .default_width(780.0)
        .show(ctx, |ui| body(ui, manager, rclone, task));
    manager.open = open;
}

/// The dialog content.
fn body(ui: &mut egui::Ui, manager: &mut Manager, rclone: &str, task: &mut TaskRunner) {
    ui.label(tr("Everything RPool stores on this provider, grouped by owner. Only data no configured pool needs can be selected: it is marked first and deleted after the grace period, when every reference is checked again."));
    ui.horizontal_wrapped(|ui| {
        if manager.crypts.len() > 1 {
            let before = manager.crypt.clone();
            egui::ComboBox::from_id_salt("provider-storage-crypt")
                .selected_text(&manager.crypt)
                .show_ui(ui, |ui| {
                    for crypt in &manager.crypts {
                        ui.selectable_value(&mut manager.crypt, crypt.clone(), crypt.as_str());
                    }
                });
            if manager.crypt != before {
                manager.reset();
            }
        } else {
            ui.monospace(&manager.crypt);
        }
        let loading = manager.view.is_loading();
        if ui
            .add_enabled(!loading, egui::Button::new(tr("Refresh")))
            .clicked()
        {
            manager.view.stale = true;
            manager.clear_preview();
        }
        if loading {
            ui.spinner();
            ui.label(tr("Reading every account and the drive metadata…"));
        }
    });
    let view = match &manager.view.value {
        Some(Ok(view)) => view.clone(),
        Some(Err(error)) => {
            ui.colored_label(
                theme::error_colors(ui.visuals().dark_mode).1,
                trf("The check failed: {error}", &[("error", error)]),
            );
            return;
        }
        None => return,
    };
    summary(ui, &view);
    ui.separator();
    table(ui, manager, &view);
    ui.separator();
    actions(ui, manager, &view, rclone, task);
}

/// Totals, pools, uncertain sources and undecryptable names.
fn summary(ui: &mut egui::Ui, view: &StorageView) {
    let pools = if view.pools.is_empty() {
        tr("no configured pool").to_string()
    } else {
        view.pools.join(", ")
    };
    ui.label(trf(
        "{remote}: {size} in {n} objects · used by {pools}",
        &[
            ("remote", &view.remote),
            ("size", &format_bytes(view.bytes)),
            ("n", &view.objects),
            ("pools", &pools),
        ],
    ));
    if !view.uncertain.is_empty() {
        ui.colored_label(
            theme::warning_colors(ui.visuals().dark_mode).1,
            tr("Some sources could not be read, so nothing can be deleted now:"),
        );
        for source in view.uncertain.iter().take(5) {
            ui.small(source);
        }
    }
    if let Some(count) = view.undecryptable.filter(|c| *c > 0) {
        theme::hint(
            ui,
            &trf(
                "{n} names on this provider do not decrypt with this crypt remote (another key or name encoding). They are never touched.",
                &[("n", &count)],
            ),
        );
    }
}

/// The groups table with a checkbox on each deletable group.
fn table(ui: &mut egui::Ui, manager: &mut Manager, view: &StorageView) {
    let now = crate::utils::now_unix();
    egui::ScrollArea::vertical()
        .id_salt("provider-storage-groups")
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new("provider-storage-grid")
                .striped(true)
                .num_columns(6)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("");
                    ui.strong(tr("Group"));
                    ui.strong(tr("Objects"));
                    ui.strong(tr("Size"));
                    ui.strong(tr("Status"));
                    ui.strong(tr("Marked"));
                    ui.end_row();
                    for group in &view.groups {
                        if group.deletable {
                            let mut on = manager.selected.contains(&group.id);
                            if ui.checkbox(&mut on, "").changed() {
                                // `actions` then forgets the acknowledgements.
                                if on {
                                    manager.selected.insert(group.id.clone());
                                } else {
                                    manager.selected.remove(&group.id);
                                }
                            }
                        } else {
                            ui.label("");
                        }
                        let title = match &group.pool {
                            Some(pool) => format!("{} · {pool}", kind_label(group.kind)),
                            None => kind_label(group.kind).to_string(),
                        };
                        ui.vertical(|ui| {
                            ui.label(title).on_hover_text(&group.reason);
                            ui.small(&group.id);
                        });
                        ui.label(group.objects.to_string());
                        ui.label(format_bytes(group.bytes));
                        ui.label(status_label(group)).on_hover_text(&group.reason);
                        ui.label(pending_label(group, now));
                        ui.end_row();
                    }
                });
        });
}

/// Acknowledgements, preview, "Mark for deletion" and "Delete now".
fn actions(
    ui: &mut egui::Ui,
    manager: &mut Manager,
    view: &StorageView,
    rclone: &str,
    task: &mut TaskRunner,
) {
    let ticked = ticked(view, &manager.selected);
    let ids: Vec<String> = ticked.iter().map(|g| g.id.clone()).collect();
    manager.sync_selection(&ids);
    let needs: BTreeSet<Ack> = ticked
        .iter()
        .flat_map(|g| g.needs.iter().copied())
        .collect();
    if needs.contains(&Ack::UnconfirmedGenerations) {
        ui.checkbox(
            &mut manager.unconfirmed,
            tr("No PC still uses these older drive generations (every drive was switched to the current one)"),
        );
    }
    if needs.contains(&Ack::ForeignPools) {
        ui.checkbox(
            &mut manager.foreign,
            tr("These other pools are not used by any PC any more"),
        );
    }
    let acked = needs.iter().all(|a| match a {
        Ack::UnconfirmedGenerations => manager.unconfirmed,
        Ack::ForeignPools => manager.foreign,
    });
    let mut choice = Choice {
        groups: ids,
        unconfirmed: manager.unconfirmed && needs.contains(&Ack::UnconfirmedGenerations),
        foreign: manager.foreign && needs.contains(&Ack::ForeignPools),
        force: false,
    };
    manager.drop_stale_preview(&choice);
    choice.force = manager.guard_passed(&choice);
    let now = crate::utils::now_unix();
    let unmarked = ticked.iter().any(|g| g.pending.marked < g.units);
    let due = ticked
        .iter()
        .any(|g| g.pending.first_due_unix.is_some_and(|d| d <= now));
    let ready = !ticked.is_empty() && acked && !task.is_running() && !manager.started;
    theme::hint(
        ui,
        &trf(
            "Marked data is deleted {n} days later, after every reference is checked again. Speed-test leftovers are deleted one hour after marking.",
            &[("n", &view.grace_days)],
        ),
    );
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                ready && !manager.preview.is_loading(),
                egui::Button::new(tr("Preview")),
            )
            .clicked()
        {
            // A new preview: the old numbers and `force` do not apply to it.
            manager.clear_preview();
            manager
                .preview
                .start(rclone, clean_args(&manager.crypt, &choice, Run::Preview));
            manager.previewed = Some(choice.clone());
        }
        if ui
            .add_enabled(
                ready && unmarked,
                egui::Button::new(tr("Mark for deletion")),
            )
            .on_hover_text(tr(
                "Publishes marks every PC sees and deletes nothing. After the grace period, Delete now deletes what is still unused.",
            ))
            .clicked()
        {
            manager.notice = Some(start(manager, rclone, task, &choice, Run::Mark));
        }
        if theme::danger_button(ui, ready && due, tr("Delete now")).clicked() {
            manager.confirm_delete = true;
        }
        if manager.preview.is_loading() {
            ui.spinner();
        }
    });
    if let Some(result) = &manager.preview.value {
        match result {
            Ok(report) => preview(ui, report, &mut manager.force),
            Err(error) => {
                ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
            }
        }
    }
    if manager.confirm_delete {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            tr("Delete the data of the ticked groups whose grace period is over? Everything is checked again first; data needed again is kept."),
        );
        ui.horizontal(|ui| {
            if theme::danger_button(ui, ready, tr("Yes, delete")).clicked() {
                manager.confirm_delete = false;
                manager.notice = Some(start(manager, rclone, task, &choice, Run::Delete));
            }
            if ui.button(tr("Cancel")).clicked() {
                manager.confirm_delete = false;
            }
        });
    }
    if let Some(notice) = &manager.notice {
        ui.label(notice);
    }
}

/// The preview's numbers; offers `--force` when the guard would stop it.
fn preview(ui: &mut egui::Ui, report: &CleanReport, force: &mut bool) {
    let line = |label: &str, bytes: u64, units: u64| {
        trf(
            "{label}: {size} ({n} units)",
            &[
                ("label", &label),
                ("size", &format_bytes(bytes)),
                ("n", &units),
            ],
        )
    };
    ui.label(line(tr("To mark"), report.new.bytes, report.new.units));
    ui.label(line(
        tr("Waiting for the grace period"),
        report.waiting.bytes,
        report.waiting.units,
    ));
    ui.label(line(
        tr("Due for deletion"),
        report.due.bytes,
        report.due.units,
    ));
    for source in &report.postponed {
        ui.small(source);
    }
    for note in &report.notes {
        ui.small(note);
    }
    if let Some(guard) = &report.guard {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            trf("Mass-delete guard: {reason}", &[("reason", guard)]),
        );
        ui.checkbox(force, tr("Delete anyway (I checked the groups)"));
    }
}

/// Starts a confirmed `provider storage clean` in the task runner.
fn start(
    manager: &mut Manager,
    rclone: &str,
    task: &mut TaskRunner,
    choice: &Choice,
    run: Run,
) -> String {
    match task.start_rpool(
        STORAGE_TASK,
        rclone,
        clean_args(&manager.crypt, choice, run),
    ) {
        Ok(()) => {
            manager.started = true;
            tr("Cleanup started. See Jobs for the result; this list refreshes when it ends.").into()
        }
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_commands_parse_and_carry_every_choice() {
        use clap::Parser;
        let parse = |args: Vec<OsString>| {
            let argv = std::iter::once(OsString::from("rpool")).chain(args);
            crate::cli::Cli::try_parse_from(argv).is_ok()
        };
        assert!(parse(list_args("gms_1_crypt:")));
        let choice = Choice {
            groups: vec!["old:RPool:first".into(), "foreign:0123".into()],
            unconfirmed: true,
            foreign: true,
            force: true,
        };
        let preview = clean_args("gms_1_crypt:", &choice, Run::Preview);
        assert!(parse(preview.clone()));
        assert!(preview.iter().any(|a| a == "--json") && !preview.iter().any(|a| a == "--force"));
        // "Mark for deletion" never deletes: --mark-only, and no --force.
        let mark = clean_args("gms_1_crypt:", &choice, Run::Mark);
        assert!(parse(mark.clone()));
        assert!(mark.iter().any(|a| a == "--confirm") && mark.iter().any(|a| a == "--mark-only"));
        assert!(!mark.iter().any(|a| a == "--force"));
        let run = clean_args("gms_1_crypt:", &choice, Run::Delete);
        assert!(parse(run.clone()));
        for flag in [
            "--confirm",
            "--force",
            "--include-unconfirmed-generations",
            "--include-foreign-pools",
        ] {
            assert!(run.iter().any(|a| a == flag), "{flag}");
        }
        assert_eq!(run.iter().filter(|a| *a == "--group").count(), 2);
        assert!(!run.iter().any(|a| a == "--mark-only"));
        // The CLI refuses --mark-only without --confirm and with --force.
        let cli = |extra: &[&str]| {
            let mut args = clean_args("gms_1_crypt:", &Choice::default(), Run::Preview);
            args.extend(extra.iter().map(OsString::from));
            parse(args)
        };
        assert!(!cli(&["--mark-only"]) && !cli(&["--confirm", "--mark-only", "--force"]));
        let manager = Manager::open_for(&["a_crypt:".into(), "b_crypt:".into()]);
        assert!(manager.open && manager.crypt == "a_crypt:");
    }

    #[test]
    fn acknowledgements_and_force_belong_to_one_selection_and_preview() {
        let mut manager = Manager::open_for(&["a_crypt:".into()]);
        let groups = vec!["old:RPool:first".to_string()];
        manager.sync_selection(&groups);
        (manager.unconfirmed, manager.foreign, manager.force) = (true, true, true);
        let choice = Choice {
            groups: groups.clone(),
            unconfirmed: true,
            ..Choice::default()
        };
        // Ticked, but no preview of this choice showed the guard: no --force.
        assert!(!manager.guard_passed(&choice));
        let mut report = CleanReport::new(
            "a_crypt:",
            crate::provider::storage::model::CleanMode::Preview,
            &groups,
            7,
        );
        manager.previewed = Some(choice.clone());
        manager.preview = Fetch::ready(report.clone());
        assert!(!manager.guard_passed(&choice), "no guard in the preview");
        report.guard = Some("60% of the objects".into());
        manager.preview = Fetch::ready(report.clone());
        assert!(manager.guard_passed(&choice));
        // The same selection keeps everything.
        manager.sync_selection(&groups);
        assert!(manager.unconfirmed && manager.guard_passed(&choice));
        // Another choice (an acknowledgement unticked): the preview and
        // `force` are dropped.
        let other = Choice {
            unconfirmed: false,
            ..choice.clone()
        };
        manager.drop_stale_preview(&other);
        assert!(!manager.force && manager.preview.value.is_none());
        assert!(!manager.guard_passed(&other) && !manager.guard_passed(&choice));
        // Another selection forgets the acknowledgements and `force`.
        manager.previewed = Some(choice.clone());
        manager.preview = Fetch::ready(report);
        (manager.unconfirmed, manager.foreign, manager.force) = (true, true, true);
        manager.confirm_delete = true;
        manager.sync_selection(&["old:RPool:first".into(), "foreign:0123".into()]);
        assert!(!manager.unconfirmed && !manager.foreign && !manager.force);
        assert!(!manager.confirm_delete && manager.previewed.is_none());
    }

    #[test]
    fn labels_cover_every_status() {
        let mut group = Group {
            id: "g".into(),
            kind: GroupKind::OldGeneration,
            pool: None,
            scope: None,
            generations: vec![],
            units: 2,
            archives: 1,
            objects: 3,
            bytes: 4,
            deletable: false,
            needs: vec![],
            grace_seconds: None,
            reason: String::new(),
            pending: Default::default(),
        };
        assert_eq!(status_label(&group), "Kept");
        group.deletable = true;
        assert_eq!(status_label(&group), "Deletable");
        group.needs = vec![Ack::ForeignPools];
        assert_eq!(status_label(&group), "Deletable after confirming");
        assert_eq!(pending_label(&group, 10), "");
        group.pending.marked = 2;
        group.pending.first_due_unix = Some(5);
        assert_eq!(pending_label(&group, 10), "2 marked · due now");
        assert!(pending_label(&group, 1).starts_with("2 marked · due "));
    }
}
