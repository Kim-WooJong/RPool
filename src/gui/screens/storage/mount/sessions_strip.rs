//! The "Mounted pools" strip: every running session (and finished ones of
//! other pools whose result is unread), with Unmount and Show.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

enum Action {
    Show(String),
    Stop(String),
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let sessions = state.mount.mounted_sessions();
    let finished: Vec<String> = state
        .mount
        .finished_background()
        .into_iter()
        .map(str::to_string)
        .collect();
    // A single session of the selected pool is already the status bar.
    if finished.is_empty() && sessions.iter().all(|s| s.selected) {
        return;
    }
    let mut action = None;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mounted = sessions.iter().filter(|s| s.mounted).count();
        ui.horizontal_wrapped(|ui| {
            ui.strong(tr("Mounted pools"));
            ui.label(
                egui::RichText::new(trf(
                    "{mounted} mounted · {running} running",
                    &[("mounted", &mounted), ("running", &sessions.len())],
                ))
                .weak(),
            );
        });
        for session in &sessions {
            ui.horizontal_wrapped(|ui| {
                let (label, tone) = if session.stopping {
                    (tr("Stopping"), StatusTone::Warning)
                } else {
                    (tr("Running"), StatusTone::Success)
                };
                status_badge(ui, label, tone);
                ui.strong(&session.pool);
                if session.mounted {
                    ui.label(format!("› {}", session.mountpoint));
                    ui.label(egui::RichText::new(session.frontend).weak());
                } else {
                    ui.label(egui::RichText::new(tr("sync or maintenance")).weak());
                }
                let stop = if session.mounted { tr("Unmount") } else { tr("Stop") };
                if ui
                    .add_enabled(!session.stopping, egui::Button::new(stop))
                    .on_hover_text(tr("Stops gracefully. Pending local changes are kept and upload on the next start or sync."))
                    .clicked()
                {
                    action = Some(Action::Stop(session.pool.clone()));
                }
                if session.selected {
                    ui.label(egui::RichText::new(tr("shown below")).weak());
                } else if ui.small_button(tr("Show")).clicked() {
                    action = Some(Action::Show(session.pool.clone()));
                }
            });
        }
        for pool in &finished {
            ui.horizontal_wrapped(|ui| {
                status_badge(ui, tr("Finished"), StatusTone::Neutral);
                ui.strong(pool);
                if ui.small_button(tr("Show")).clicked() {
                    action = Some(Action::Show(pool.clone()));
                }
            });
        }
    });
    match action {
        Some(Action::Show(pool)) => state.mount.select_pool(pool, &mut state.settings),
        Some(Action::Stop(pool)) => {
            if let Err(error) = state.mount.stop_pool(&pool) {
                state.mount.session.notice = Some(error);
            }
        }
        None => {}
    }
    ui.add_space(theme::SUBSECTION_GAP);
}
