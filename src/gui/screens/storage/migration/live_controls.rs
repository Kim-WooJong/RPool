//! Run step: change archives at once, shards at once and the bandwidth limit
//! of this window's running migration without restarting it (the run's live
//! controls, `migration::live_control`). Shows what the run has in force, the
//! run's ack of each change and when each value takes effect.
use super::state::MigrationForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::migration::live_control::{self, AckState, RunState, Settings};
use eframe::egui;
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the run's state file is read while the step is shown.
const REFRESH: Duration = Duration::from_millis(500);
/// How long to wait for a new run's first state before calling it unsupported.
const FIRST_REPORT: Duration = Duration::from_secs(15);

/// What the run step knows about the live run of this window.
#[derive(Debug, Default)]
pub(crate) struct LiveView {
    /// Last state read from the run's control folder.
    state: Option<RunState>,
    /// When it was read.
    read_at: Option<Instant>,
    /// When this window started the run.
    since: Option<Instant>,
    /// Sequence number of the change waiting for the run's ack.
    pending: Option<u64>,
    /// Outcome of the last change: (applied, message).
    outcome: Option<(bool, String)>,
}

impl LiveView {
    /// Forgets the previous run (a new run starts, or the run ended).
    pub(crate) fn reset(&mut self, started: bool) {
        *self = Self {
            since: started.then(Instant::now),
            ..Self::default()
        };
    }

    /// Reads the run's state (at most every [`REFRESH`]) and settles the
    /// pending change when the run has decided it.
    fn refresh(&mut self, dir: &Path) {
        if self.read_at.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        self.read_at = Some(Instant::now());
        // A failed read keeps the last state; the heartbeat ages it out.
        if let Ok(state) = live_control::read_state(dir) {
            self.state = state;
        }
        let (Some(seq), Some(state)) = (self.pending, &self.state) else {
            return;
        };
        match state.ack_of(seq) {
            AckState::Pending => {}
            AckState::Applied => {
                self.pending = None;
                self.outcome = Some((true, tr("Applied to the running migration.").into()));
            }
            AckState::Rejected(error) => {
                self.pending = None;
                self.outcome = Some((
                    false,
                    trf(
                        "Rejected by the run; the previous values stay: {error}",
                        &[("error", &error)],
                    ),
                ));
            }
            AckState::Superseded => {
                self.pending = None;
                self.outcome = None;
            }
        }
    }
}

/// "no limit" or the bandwidth text.
fn bandwidth_text(text: &str) -> String {
    if text.trim().is_empty() {
        tr("no limit").to_string()
    } else {
        text.trim().to_string()
    }
}

/// The live-controls panel of the running migration `id` (nothing when this
/// window did not start the run).
pub(super) fn show(ui: &mut egui::Ui, form: &mut MigrationForm, id: &str) {
    let Some(dir) = form.run_control_dir().map(Path::to_path_buf) else {
        return;
    };
    form.live.refresh(&dir);
    ui.ctx().request_repaint_after(REFRESH);
    ui.separator();
    ui.strong(tr("Change while running"));
    let now = crate::utils::now_unix();
    let warn = ui.visuals().warn_fg_color;
    let state = match form.live.state.clone() {
        None => {
            if form.live.since.is_some_and(|s| s.elapsed() < FIRST_REPORT) {
                theme::hint(ui, tr("Waiting for the run to report its live settings…"));
            } else {
                ui.colored_label(warn, tr("This run does not report live settings, so nothing can be changed while it runs. New values take effect at the next resume."));
            }
            return;
        }
        Some(state) if state.migration_id != id => {
            ui.colored_label(warn, tr("The control folder reports another migration; nothing can be applied."));
            return;
        }
        Some(state) if !state.is_live(now) => {
            ui.colored_label(warn, tr("The run stopped reporting (ended or not responding); changes cannot be applied to it."));
            return;
        }
        Some(state) => state,
    };
    theme::hint(
        ui,
        &trf(
            "In force: {parallel} archives at once ({active} in progress), {transfers} shards at once for archives that start now, bandwidth {bandwidth}.",
            &[
                ("parallel", &state.applied.parallel),
                ("active", &state.active_archives),
                ("transfers", &state.effective_transfers()),
                ("bandwidth", &bandwidth_text(&state.applied.bwlimit)),
            ],
        ),
    );
    let (settings, invalid) = ui.horizontal_wrapped(|ui| {
        let hint = tr("Applies when the next archive starts. Lowering it never stops archives in progress; it only holds new ones back until fewer run.");
        ui.label(tr("Archives at once")).on_hover_text(hint);
        ui.add(
            egui::DragValue::new(&mut form.parallel.0)
                .range(1..=crate::migration::execute::MAX_PARALLEL),
        )
        .on_hover_text(hint);
        let hint = tr("Applies to archives that start after the change; archives in progress keep their count. 0 = the pool's workers.");
        ui.label(tr("Shards at once")).on_hover_text(hint);
        ui.add(egui::DragValue::new(&mut form.transfers).range(0..=256))
            .on_hover_text(hint);
        let hint = tr("RPool's own transfers follow within about a second, open ones included; copies rclone makes between accounts by itself use the new limit from their next copy. Empty = no limit.");
        ui.label(tr("Bandwidth")).on_hover_text(hint);
        ui.add(
            egui::TextEdit::singleline(&mut form.bwlimit)
                .hint_text("01:00,20M 08:00,2M")
                .desired_width(160.0),
        )
        .on_hover_text(hint);
        // Read the inputs after the widgets process this frame, so the
        // submitted values and validation match what is currently displayed.
        let settings = Settings {
            parallel: form.parallel.0,
            transfers: form.transfers as usize,
            bwlimit: form.bwlimit.clone(),
        }
        .normalized();
        let invalid = settings.validate().err().map(|e| format!("{e:#}"));
        if theme::primary_button(ui, invalid.is_none(), tr("Apply to this run")).clicked() {
            match live_control::submit(&dir, &state, settings.clone()) {
                Ok(seq) => {
                    form.live.pending = Some(seq);
                    form.live.outcome = None;
                    form.live.read_at = None;
                }
                Err(error) => {
                    form.live.outcome = Some((
                        false,
                        trf("Not applied: {error}", &[("error", &format!("{error:#}"))]),
                    ));
                }
            }
        }
        (settings, invalid)
    }).inner;
    if let Some(error) = &invalid {
        ui.colored_label(
            ui.visuals().error_fg_color,
            trf("Not valid, so it cannot be applied: {error}", &[("error", error)]),
        );
    } else if form.live.pending.is_none() && settings != state.applied {
        theme::hint(ui, tr("Edited values are not applied yet."));
    }
    if form.live.pending.is_some() {
        theme::hint(ui, tr("Sent; waiting for the run to confirm…"));
    } else if let Some((ok, message)) = &form.live.outcome {
        if *ok {
            theme::hint(ui, message);
        } else {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }
    theme::hint(ui, tr("Archives at once: next archive start; archives in progress are never cancelled. Shards at once: archives that start afterwards. Bandwidth: RPool's own transfers within about a second; rclone copies between accounts from their next copy."));
}
