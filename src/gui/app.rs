use crate::gui::i18n::{tr, trf};
use crate::gui::navigation;
use crate::gui::screens::{dashboard, files, jobs, maintenance, monitoring, settings, storage};
use crate::gui::state::{self, GuiState, Page};
use crate::gui::task::{JobStatus, TaskRunner};
use crate::gui::theme;
use crate::gui::usage_refresh::UsageRefresh;
use crate::gui::widgets::task_console;
use anyhow::{anyhow, Result};
use eframe::egui;
use std::collections::BTreeSet;
use std::time::Duration;

pub(crate) const AUTO_ENCRYPTION_TASK: &str = "Ensure provider encryption";

#[derive(Default)]
struct EncryptionSetup {
    attempted: BTreeSet<(String, String)>,
    pending: Option<(String, String)>,
}

impl EncryptionSetup {
    fn discover(&mut self, rclone: String, signature: String, has_providers: bool) {
        let key = (rclone, signature);
        self.pending = (has_providers && !self.attempted.contains(&key)).then_some(key);
    }

    fn take_ready(&mut self, busy: bool) -> Option<String> {
        if busy {
            return None;
        }
        let key = self.pending.take()?;
        let rclone = key.0.clone();
        self.attempted.insert(key);
        Some(rclone)
    }
}

pub(crate) fn launch(startup_rclone: &str) -> Result<()> {
    let startup_rclone = startup_rclone.to_string();
    let native_options = eframe::NativeOptions {
        // Select explicitly: enabling another backend later must not silently change this.
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 800.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rpool storage console",
        native_options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let app = RpoolGui::new(&startup_rclone);
            super::i18n::fonts::install(&cc.egui_ctx, super::i18n::language());
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

struct RpoolGui {
    state: GuiState,
    task: TaskRunner,
    usage: UsageRefresh,
    refresh_pending: bool,
    encryption: EncryptionSetup,
    discovery_rclone: String,
    /// `None` follows the window height; a click sets it explicitly.
    console_open: Option<bool>,
    #[cfg(debug_assertions)]
    snapshots: Option<super::snapshot::Snapshots>,
}

impl RpoolGui {
    fn new(startup_rclone: &str) -> Self {
        let state = state::load(startup_rclone);
        super::i18n::set_language(state.settings.language);
        let mut usage = UsageRefresh::default();
        usage.start(state.settings.rclone.clone(), state.settings.workers);
        Self {
            discovery_rclone: state.settings.rclone.clone(),
            encryption: EncryptionSetup::default(),
            state,
            task: TaskRunner::default(),
            usage,
            refresh_pending: false,
            console_open: None,
            #[cfg(debug_assertions)]
            snapshots: super::snapshot::Snapshots::from_env(),
        }
    }

    fn poll_background(&mut self) {
        self.state.mount.poll();
        // Keeps the live graphs filling while another page is shown.
        self.state.monitoring.poll(std::time::Instant::now());
        let connection_result = self
            .state
            .providers
            .connection
            .as_mut()
            .and_then(|connection| connection.poll());
        if let Some(result) = connection_result {
            self.state.providers.connection = None;
            self.refresh_pending = true;
            self.state.providers.setup_notice = Some(match result {
                Ok(()) => tr("Connection wizard closed. Refreshing providers and checking encryption automatically.").into(),
                Err(error) => trf("Connection watcher stopped: {error}. Refreshing provider discovery.", &[("error", &error)]),
            });
        }
        let automatic = self.task.task_name() == Some(AUTO_ENCRYPTION_TASK);
        let provisioning = self.task.task_name() == Some("Create encrypted provider");
        let task_finished = self.task.poll();
        if provisioning && task_finished == Some(JobStatus::Completed) {
            self.refresh_pending = true;
            self.state.providers.setup_notice = Some(tr("Encrypted provider created. Provider list refreshed automatically; back up your rclone configuration before uploading.").into());
        }
        if automatic && task_finished.is_some() {
            self.state.providers.encryption_failed = task_finished != Some(JobStatus::Completed);
            // Even a failed/cancelled run may have provisioned some providers.
            self.refresh_pending = true;
            self.state.providers.setup_notice = Some(
                if task_finished == Some(JobStatus::Completed) {
                    tr("Provider encryption checked automatically. Back up your rclone configuration to preserve the encryption keys.").into()
                } else {
                    tr("Automatic encryption setup did not complete. See Jobs for details, then use Retry automatic encryption in Providers after resolving the error.").into()
                },
            );
        }
        if let Some(status) = task_finished {
            storage::pools::handle_task_completion(&mut self.state, &self.task, status);
            storage::reprocess::handle_task_completion(&mut self.state, &self.task, status);
            storage::speed_test::handle_task_completion(&mut self.state, &self.task, status);
            maintenance::handle_task_completion(&mut self.state, &self.task, status);
        }
        files::upload::poll_batch(&mut self.state, &mut self.task);
        if task_finished.is_some() {
            self.state.dashboard.refresh();
        }
        if let Some(result) = self.usage.poll() {
            match result {
                Ok(snapshot) => {
                    self.encryption.discover(
                        self.discovery_rclone.clone(),
                        snapshot.catalog_signature,
                        snapshot.needs_encryption,
                    );
                    self.state.usage_reports = snapshot.reports;
                    self.state.crypt_remotes = snapshot.crypt_remotes;
                    self.state.backing_remotes = snapshot.backing_remotes;
                    self.state.providers.missing_encryption = snapshot.missing_encryption;
                    self.state.providers.discovery_known = true;
                    self.state.usage_error = snapshot.warning;
                }
                Err(error) => {
                    self.state.providers.discovery_known = false;
                    self.state.usage_error = Some(error);
                }
            }
        }
        if self.discovery_rclone != self.state.settings.rclone {
            self.encryption.pending = None;
            self.refresh_pending = true;
        }
        if let Some(rclone) = self.encryption.take_ready(
            self.task.is_running()
                || self.usage.is_running()
                || self.refresh_pending
                || self.state.providers.connection.is_some(),
        ) {
            debug_assert_eq!(rclone, self.state.settings.rclone);
            self.state.providers.encryption_failed = false;
            if let Err(error) =
                storage::providers::start_automatic_encryption(&self.state, &mut self.task)
            {
                self.state.providers.encryption_failed = true;
                self.state.providers.setup_notice = Some(error);
            }
        }
    }
}

impl eframe::App for RpoolGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_background();
        // Closing asks every mounted pool to unmount gracefully; the child
        // processes finish their writes after the window is gone.
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            self.state.mount.stop_all();
        }
        #[cfg(debug_assertions)]
        if let Some(snapshots) = &mut self.snapshots {
            snapshots.tick(ui.ctx(), &mut self.state);
        }

        if self.task.is_running()
            || self.state.mount.any_running()
            || self.usage.is_running()
            || self.state.providers.connection.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if !self.state.monitoring.mounts.is_empty() {
            ui.ctx()
                .request_repaint_after(monitoring::state_poll_interval());
        }

        let window = ui.ctx().content_rect();
        let p = theme::pal(ui);
        let compact = window.width() < theme::NAV_COMPACT_BELOW;
        egui::Panel::left(if compact {
            "navigation-compact"
        } else {
            "navigation"
        })
        .resizable(false)
        .exact_size(if compact {
            theme::NAVIGATION_COMPACT_WIDTH
        } else {
            theme::NAVIGATION_WIDTH
        })
        .frame(
            egui::Frame::new()
                .fill(p.nav)
                .inner_margin(egui::Margin::symmetric(8, 8)),
        )
        .show(ui, |ui| navigation::show(ui, &mut self.state.page, compact));

        egui::Panel::top("top-bar")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(page_title(self.state.page))
                            .size(15.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::widgets::global_theme_preference_switch(ui);
                        mount_badge(ui, &self.state);
                        if self.task.is_running() {
                            ui.label(self.task.task_name().unwrap_or(tr("operation")));
                            ui.spinner();
                        }
                    });
                });
            });

        // The console starts collapsed on short windows and opens while a
        // task runs, unless the user chose otherwise.
        let open = self
            .console_open
            .unwrap_or(window.height() >= theme::CONSOLE_MINI_BELOW || self.task.is_running());
        if open {
            egui::Panel::bottom("task-console")
                .resizable(true)
                .default_size(theme::TASK_CONSOLE_HEIGHT)
                .size_range(110.0..=(window.height() * 0.5).max(120.0))
                .show(ui, |ui| {
                    if task_console(ui, &mut self.task, true) {
                        self.console_open = Some(false);
                    }
                });
        } else {
            egui::Panel::bottom("task-console-mini")
                .resizable(false)
                .exact_size(theme::TASK_CONSOLE_MINI_HEIGHT)
                .show(ui, |ui| {
                    if task_console(ui, &mut self.task, false) {
                        self.console_open = Some(true);
                    }
                });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.state.page {
            Page::Dashboard => dashboard::show(ui, &mut self.state, &mut self.usage),
            Page::Drive => storage::mount::show(ui, &mut self.state),
            Page::Files => files::show(ui, &mut self.state, &mut self.task),
            Page::Storage => storage::show(ui, &mut self.state, &mut self.task),
            Page::Monitoring => monitoring::show(ui, &mut self.state),
            Page::Jobs => jobs::show(ui, &mut self.state, &mut self.task),
            Page::Maintenance => maintenance::show(ui, &mut self.state, &mut self.task),
            Page::Settings => settings::show(ui, &mut self.state, &mut self.task),
        });
        self.refresh_pending |= std::mem::take(&mut self.state.providers.refresh_requested);
        self.refresh_pending |= std::mem::take(&mut self.state.pools.refresh_requested);
        if self.refresh_pending && !self.usage.is_running() {
            self.refresh_pending = false;
            self.discovery_rclone = self.state.settings.rclone.clone();
            self.usage.start(
                self.state.settings.rclone.clone(),
                self.state.settings.workers,
            );
            ui.ctx().request_repaint();
        }
    }
}

/// How many drives are mounted (and which pool the Drive page shows), or
/// that a drive sync/maintenance run is busy.
fn mount_badge(ui: &mut egui::Ui, state: &GuiState) {
    use crate::gui::widgets::{status_badge, StatusTone};
    let sessions = storage::mount::mounted_sessions(state);
    let mounted: Vec<_> = sessions.iter().filter(|s| s.mounted).collect();
    let pools = sessions
        .iter()
        .map(|s| s.pool.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let (label, tone) = match mounted.as_slice() {
        [] if sessions.is_empty() => return,
        [] => (tr("Drive task running").to_string(), StatusTone::Neutral),
        [_] => (tr("Drive mounted").to_string(), StatusTone::Success),
        _ => {
            let shown = sessions
                .iter()
                .find(|s| s.selected)
                .map_or("", |s| s.pool.as_str());
            let label = if shown.is_empty() {
                trf("{n} drives mounted", &[("n", &mounted.len())])
            } else {
                trf(
                    "{n} drives mounted · {pool}",
                    &[("n", &mounted.len()), ("pool", &shown)],
                )
            };
            (label, StatusTone::Success)
        }
    };
    ui.scope(|ui| status_badge(ui, &label, tone))
        .response
        .on_hover_text(pools);
}

fn page_title(page: Page) -> &'static str {
    match page {
        Page::Dashboard => tr("Overview"),
        Page::Drive => tr("Drive"),
        Page::Files => tr("Files"),
        Page::Storage => tr("Storage"),
        Page::Monitoring => tr("Monitoring"),
        Page::Maintenance => tr("Health"),
        Page::Jobs => tr("Activity"),
        Page::Settings => tr("Settings"),
    }
}

#[cfg(test)]
mod tests {
    use super::EncryptionSetup;

    #[test]
    fn automatic_setup_waits_for_idle_and_does_not_repeat_failed_catalog() {
        let mut setup = EncryptionSetup::default();
        setup.discover("rclone".into(), "catalog-a".into(), true);
        assert!(setup.take_ready(true).is_none());
        assert_eq!(setup.take_ready(false).as_deref(), Some("rclone"));
        setup.discover("rclone".into(), "catalog-a".into(), true);
        assert!(setup.take_ready(false).is_none());
        setup.discover("rclone".into(), "catalog-b".into(), true);
        assert_eq!(setup.take_ready(false).as_deref(), Some("rclone"));
    }

    #[test]
    fn fully_covered_discovery_clears_pending_and_rclone_change_rechecks() {
        let mut setup = EncryptionSetup::default();
        setup.discover("rclone".into(), "catalog".into(), true);
        setup.discover("rclone".into(), "catalog".into(), false);
        assert!(setup.take_ready(false).is_none());
        setup.discover("rclone".into(), "catalog".into(), true);
        assert!(setup.take_ready(false).is_some());
        setup.discover("other-rclone".into(), "catalog".into(), true);
        assert_eq!(setup.take_ready(false).as_deref(), Some("other-rclone"));
    }
}
