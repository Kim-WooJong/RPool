use crate::gui::navigation;
use crate::gui::screens::{dashboard, files, jobs, maintenance, settings, storage};
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
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rpool storage console",
        native_options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(RpoolGui::new(&startup_rclone)))
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
}

impl RpoolGui {
    fn new(startup_rclone: &str) -> Self {
        let state = state::load(startup_rclone);
        let mut usage = UsageRefresh::default();
        usage.start(state.settings.rclone.clone(), state.settings.workers);
        Self {
            discovery_rclone: state.settings.rclone.clone(),
            encryption: EncryptionSetup::default(),
            state,
            task: TaskRunner::default(),
            usage,
            refresh_pending: false,
        }
    }

    fn poll_background(&mut self) {
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
                Ok(()) => "Connection wizard closed. Refreshing providers and checking encryption automatically.".into(),
                Err(error) => format!("Connection watcher stopped: {error}. Refreshing provider discovery."),
            });
        }
        let automatic = self.task.task_name() == Some(AUTO_ENCRYPTION_TASK);
        let provisioning = self.task.task_name() == Some("Create encrypted provider");
        let task_finished = self.task.poll();
        if provisioning && task_finished == Some(JobStatus::Completed) {
            self.refresh_pending = true;
            self.state.providers.setup_notice = Some("Encrypted provider created. Provider list refreshed automatically; back up your rclone configuration before uploading.".into());
        }
        if automatic && task_finished.is_some() {
            self.state.providers.encryption_failed = task_finished != Some(JobStatus::Completed);
            // Even a failed/cancelled run may have provisioned some providers.
            self.refresh_pending = true;
            self.state.providers.setup_notice = Some(
                if task_finished == Some(JobStatus::Completed) {
                    "Provider encryption checked automatically. Back up your rclone configuration to preserve the encryption keys.".into()
                } else {
                    "Automatic encryption setup did not complete. See Jobs for details, then use Retry automatic encryption in Providers after resolving the error.".into()
                },
            );
        }
        if let Some(status) = task_finished {
            storage::pools::handle_task_completion(&mut self.state, &self.task, status);
            storage::reprocess::handle_task_completion(&mut self.state, &self.task, status);
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

        if self.task.is_running()
            || self.usage.is_running()
            || self.state.providers.connection.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::top("top-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("RPool").strong());
                ui.label(egui::RichText::new("Storage console").weak());
                if self.task.is_running() {
                    ui.separator();
                    ui.spinner();
                    ui.label(self.task.task_name().unwrap_or("operation"));
                }
            });
        });

        egui::Panel::left("navigation")
            .resizable(false)
            .default_size(theme::NAVIGATION_WIDTH)
            .show(ui, |ui| navigation::show(ui, &mut self.state.page));

        egui::Panel::bottom("task-console")
            .resizable(true)
            .default_size(theme::TASK_CONSOLE_HEIGHT)
            .min_size(110.0)
            .show(ui, |ui| task_console(ui, &mut self.task));

        egui::CentralPanel::default().show(ui, |ui| match self.state.page {
            Page::Dashboard => dashboard::show(ui, &mut self.state, &mut self.usage),
            Page::Files => files::show(ui, &mut self.state, &mut self.task),
            Page::Storage => storage::show(ui, &mut self.state, &mut self.task),
            Page::Jobs => jobs::show(ui, &mut self.state, &mut self.task),
            Page::Maintenance => maintenance::show(ui, &mut self.state, &mut self.task),
            Page::Settings => settings::show(ui, &mut self.state),
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
