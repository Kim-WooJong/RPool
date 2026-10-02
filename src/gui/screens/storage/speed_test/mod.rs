//! "Speed test" card: finds the slow account of a pool (Pools page) or of
//! ticked encrypted remotes (Providers page) by running
//! `rpool pool|provider speed-test … --json` and showing its report.
mod form;
mod plan;
mod results;
mod view;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) use form::{SpeedTestForm, Target};
#[cfg(debug_assertions)]
pub(crate) use view::ReportView;

use crate::gui::i18n::{duration_text, tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::{JobStatus, TaskRunner};
use crate::gui::theme;
use eframe::egui;
use plan::{
    binary_size, Plan, PlanError, Preset, LARGE_SIZES_MIB, MAX_FILES, MAX_SIZE_MIB, SMALL_COUNTS,
};

/// Task name; identifies a finished speed test (not shown translated).
const TASK: &str = "Speed test";

/// The card for the pool loaded (or selected) on the Pools page.
pub(crate) fn pool_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let saved = [state.pools.name.trim(), state.pools.selected.as_str()]
        .into_iter()
        .find_map(|name| {
            state
                .pool_definitions
                .get(name)
                .map(|pool| (name.to_string(), pool.remotes.len()))
        });
    theme::card_section(
        ui,
        tr("Speed test"),
        Some(tr("Writes random test files to every account of this pool, reads them back and deletes them. Accounts are tested one after another.")),
        |_| {},
        |ui| {
            let Some((name, accounts)) = saved else {
                theme::hint(ui, tr("Select and load a saved pool to test its accounts."));
                return;
            };
            ui.small(trf("Pool '{name}', {n} accounts", &[("name", &name), ("n", &accounts)]));
            let target = Target::Pool(name);
            body(ui, &mut state.speed_test, task, &state.settings.rclone, &target, accounts);
        },
    );
}

/// The card on the Providers page: tests the ticked encrypted remotes.
pub(crate) fn providers_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(
        ui,
        tr("Speed test"),
        Some(tr("Writes random test files to each ticked remote, reads them back and deletes them. Remotes are tested one after another.")),
        |_| {},
        |ui| {
            if state.crypt_remotes.is_empty() {
                theme::hint(ui, tr("No encrypted providers found yet."));
                return;
            }
            let form = &mut state.speed_test;
            form.remotes.retain(|r| state.crypt_remotes.contains(r));
            egui::ScrollArea::vertical()
                .id_salt("speed-test-remotes")
                .max_height(140.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for remote in &state.crypt_remotes {
                            let mut on = form.remotes.contains(remote);
                            if ui.checkbox(&mut on, remote).changed() {
                                if on {
                                    form.remotes.push(remote.clone());
                                } else {
                                    form.remotes.retain(|r| r != remote);
                                }
                            }
                        }
                    });
                });
            let accounts = form.remotes.len();
            if accounts == 0 {
                theme::hint(ui, tr("Tick the remotes to test."));
            }
            body(ui, form, task, &state.settings.rclone, &Target::Remotes, accounts);
        },
    );
}

fn plan_error_text(error: PlanError) -> String {
    match error {
        PlanError::Size => trf("Size must be 1 to {max} MiB.", &[("max", &MAX_SIZE_MIB)]),
        PlanError::Files => trf("Files must be 1 to {max}.", &[("max", &MAX_FILES)]),
        PlanError::FileTooSmall => {
            tr("Each file must be at least 4 KiB; use fewer files or a larger size.").into()
        }
    }
}

fn plan_controls(ui: &mut egui::Ui, form: &mut SpeedTestForm, enabled: bool) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (preset, label) in [
                (Preset::Quick, tr("Quick")),
                (Preset::LargeFile, tr("Large file")),
                (Preset::ManySmall, tr("Many small files")),
                (Preset::Custom, tr("Custom")),
            ] {
                ui.selectable_value(&mut form.preset, preset, label);
            }
        });
        ui.horizontal_wrapped(|ui| match form.preset {
            Preset::Quick => theme::hint(
                ui,
                tr("4 files, 16 MiB per account. Finds a slow account in about a minute."),
            ),
            Preset::LargeFile => {
                ui.label(tr("File size"));
                egui::ComboBox::from_id_salt("speed-test-large")
                    .selected_text(binary_size(form.large_mib * plan::MIB))
                    .show_ui(ui, |ui| {
                        for mib in LARGE_SIZES_MIB {
                            ui.selectable_value(
                                &mut form.large_mib,
                                mib,
                                binary_size(mib * plan::MIB),
                            );
                        }
                    });
            }
            Preset::ManySmall => {
                ui.label(tr("Files of 64 KiB"));
                egui::ComboBox::from_id_salt("speed-test-small")
                    .selected_text(form.small_count.to_string())
                    .show_ui(ui, |ui| {
                        for count in SMALL_COUNTS {
                            ui.selectable_value(&mut form.small_count, count, count.to_string());
                        }
                    });
            }
            Preset::Custom => {
                ui.label(tr("Size per account (MiB)"));
                ui.add(egui::DragValue::new(&mut form.custom.size_mib).range(1..=MAX_SIZE_MIB));
                ui.label(tr("Number of files"));
                ui.add(egui::DragValue::new(&mut form.custom.files).range(1..=MAX_FILES));
            }
        });
        ui.checkbox(&mut form.tune_uploads, tr("Find the best number of simultaneous uploads"))
            .on_hover_text(tr("After the test, uploads 1 MiB files to each account at 1, 2, 4, … 32 at once and recommends its Simultaneous uploads limit. Adds up to 128 MiB of uploads per account."));
    });
}

/// What the run will write, plus a duration warning for large runs.
fn summary(ui: &mut egui::Ui, plan: Plan, accounts: usize) {
    let per_account = binary_size(plan.bytes());
    let total = binary_size(plan.bytes().saturating_mul(accounts as u64));
    ui.small(trf(
        "{mode} per account ({size}); {total} in total for {n} accounts, written and read back.",
        &[
            ("mode", &plan.mode_label()),
            ("size", &per_account),
            ("total", &total),
            ("n", &accounts),
        ],
    ));
    if plan.is_large() && accounts > 0 {
        // Up and down, one account after another, at 50 or 5 MB/s.
        let moved = 2.0 * plan.bytes() as f64 * accounts as f64;
        let duration = duration_text(&crate::migration::speed::format_estimate(Some((
            moved / 50e6,
            moved / 5e6,
        ))));
        let (fill, fg) = theme::warning_colors(ui.visuals().dark_mode);
        egui::Frame::new().fill(fill).corner_radius(theme::CORNER_RADIUS).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(fg, trf(
                "About {size} will be uploaded and downloaded per account; this can take a long time on slow accounts ({duration} at 5–50 MB/s).",
                &[("size", &per_account), ("duration", &duration)],
            ));
        });
    }
}

fn body(
    ui: &mut egui::Ui,
    form: &mut SpeedTestForm,
    task: &mut TaskRunner,
    rclone: &str,
    target: &Target,
    accounts: usize,
) {
    let running_here = task.is_running() && form.running.as_ref() == Some(target);
    plan_controls(ui, form, !task.is_running());
    let plan = form.plan().validate();
    match plan {
        Ok(plan) => summary(ui, plan, accounts),
        Err(error) => {
            ui.colored_label(ui.visuals().error_fg_color, plan_error_text(error));
        }
    }
    let ready = plan.is_ok() && accounts > 0 && !task.is_running();
    let still_large = plan.is_ok_and(Plan::needs_confirmation);
    if form.confirming.as_ref() == Some(target) && !(ready && still_large) {
        form.confirming = None;
    }
    ui.horizontal_wrapped(|ui| {
        if running_here {
            if ui.button(tr("Cancel")).clicked() {
                task.cancel();
            }
        } else if form.confirming.as_ref() == Some(target) {
            if theme::primary_button(ui, true, tr("Start large test")).clicked() {
                if let Ok(plan) = plan {
                    start(form, task, rclone, target, plan);
                }
            }
            if ui.button(tr("Back")).clicked() {
                form.confirming = None;
            }
        } else if theme::primary_button(ui, ready, tr("Run speed test")).clicked() {
            if let Ok(plan) = plan {
                if plan.needs_confirmation() {
                    form.confirming = Some(target.clone());
                } else {
                    start(form, task, rclone, target, plan);
                }
            }
        }
    });
    if form.confirming.as_ref() == Some(target) {
        theme::hint(
            ui,
            tr("This is a large test. Click Start large test to confirm."),
        );
    }
    if running_here {
        if let Some(current) = task.current_task() {
            crate::gui::widgets::progress_view(ui, &current.progress);
        }
        let last =
            task.logs().iter().rev().find(|line| {
                !line.text.trim_start().starts_with('{') && !line.text.trim().is_empty()
            });
        match last {
            Some(line) => theme::hint(ui, &line.text),
            None => theme::hint(ui, tr("Speed test running…")),
        }
    }
    if let Some(notice) = form.notices.get(target) {
        ui.label(notice);
    }
    let apply = form
        .results
        .get(target)
        .and_then(|view| results::show(ui, view));
    if let Some(apply) = apply {
        let notice = match save_max_uploads(&apply.account, apply.uploads) {
            Ok(()) => {
                if let Some(view) = form.results.get_mut(target) {
                    view.applied(&apply.account, apply.uploads);
                }
                trf(
                    "Simultaneous uploads of {account} set to {n}.",
                    &[("account", &apply.account), ("n", &apply.uploads)],
                )
            }
            Err(error) => error,
        };
        form.notices.insert(target.clone(), notice);
    }
}

/// Sets one account's "Simultaneous uploads" limit, keeping its other limits.
fn save_max_uploads(account: &str, uploads: usize) -> Result<(), String> {
    use crate::storage::account::{edit, store};
    let uploads = u32::try_from(uploads).map_err(|e| e.to_string())?;
    let mut limits = store::load_limits().map_err(|e| format!("{e:#}"))?;
    let change = edit::LimitEdit {
        max_uploads: Some(uploads),
        ..Default::default()
    };
    edit::apply(&mut limits, account, &change).map_err(|e| format!("{e:#}"))?;
    store::save_limits(&limits).map_err(|e| format!("{e:#}"))?;
    Ok(())
}

fn args(target: &Target, remotes: &[String], plan: Plan, tune: bool) -> Vec<std::ffi::OsString> {
    let mut args = match target {
        Target::Pool(name) => plan::pool_args(name, plan),
        Target::Remotes => plan::provider_args(remotes, plan),
    };
    if tune {
        // Right after `pool|provider speed-test`, before any `--`.
        args.insert(2, "--tune-uploads".into());
    }
    args
}

fn start(
    form: &mut SpeedTestForm,
    task: &mut TaskRunner,
    rclone: &str,
    target: &Target,
    plan: Plan,
) {
    form.confirming = None;
    match task.start_rpool(
        TASK,
        rclone,
        args(target, &form.remotes, plan, form.tune_uploads),
    ) {
        Ok(()) => {
            form.running = Some(target.clone());
            form.notices.remove(target);
        }
        Err(error) => {
            form.notices.insert(target.clone(), error);
        }
    }
}

/// After any task: store the report of a finished speed test.
pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    if task.last_task().is_none_or(|last| last.name != TASK) {
        return;
    }
    let Some(target) = state.speed_test.running.take() else {
        return;
    };
    let form = &mut state.speed_test;
    let report = view::parse_report(task.logs());
    let notice = match (&report, status) {
        (Some(_), JobStatus::Completed) => None,
        (Some(_), _) => {
            Some(tr("Some accounts failed the speed test; see the errors below.").to_string())
        }
        (None, JobStatus::Cancelled) => Some(trf(
            "Speed test was cancelled. Test files may be left in a '{dir}' folder on the accounts.",
            &[("dir", &crate::speedtest::model::TEST_DIR)],
        )),
        (None, _) => Some(tr("Speed test failed; see Jobs for details.").to_string()),
    };
    if let Some(report) = &report {
        form.results
            .insert(target.clone(), view::ReportView::new(report));
    }
    match notice {
        Some(notice) => form.notices.insert(target, notice),
        None => form.notices.remove(&target),
    };
}
