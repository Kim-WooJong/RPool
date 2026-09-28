use crate::gui::navigation;
use crate::gui::screens::{dashboard, files, jobs, maintenance, settings, storage};
use crate::gui::state::{self, GuiState, Page};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::usage_refresh::UsageRefresh;
use crate::gui::widgets::task_console;
use anyhow::{anyhow, Result};
use eframe::egui;
use std::time::Duration;

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
}

impl RpoolGui {
    fn new(startup_rclone: &str) -> Self {
        let state = state::load(startup_rclone);
        let mut usage = UsageRefresh::default();
        usage.start(state.settings.rclone.clone(), state.settings.workers);
        Self {
            state,
            task: TaskRunner::default(),
            usage,
        }
    }

    fn poll_background(&mut self) {
        let task_finished = self.task.poll();
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
                    self.state.usage_reports = snapshot.reports;
                    self.state.crypt_remotes = snapshot.crypt_remotes;
                    self.state.usage_error = None;
                }
                Err(error) => self.state.usage_error = Some(error),
            }
        }
    }
}

impl eframe::App for RpoolGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_background();

        if self.task.is_running() || self.usage.is_running() {
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
    }
}
