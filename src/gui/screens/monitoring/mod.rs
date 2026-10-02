//! "Monitoring" page: live and historical network traffic of every mounted
//! pool, per cloud account. Lists exactly the mounts `crate::monitor`
//! finds running (also ones started from the CLI); nothing for unmounted
//! pools.
mod chart;
mod format;
mod history;
mod history_view;
mod live_view;
mod ring;
#[cfg(any(test, debug_assertions))]
pub(crate) mod sample;
mod source;
mod state;
mod view_model;

#[cfg(test)]
mod tests;

pub(crate) use state::{CardTab, MonitoringState};

use crate::gui::i18n::tr;
use crate::gui::state::{GuiState, Page};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;
use std::time::{Duration, Instant};

/// How often the app should wake while a pool is mounted, so the live
/// samples keep coming in on other pages too.
pub(crate) fn state_poll_interval() -> Duration {
    state::STATUS_EVERY
}

/// Draws the Monitoring page: polls `state.monitoring`, keeps repainting
/// while traffic moves, and shows one card per running mount (or a hint with
/// an Open Drive button when none). Called by the app for the Monitoring page.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let now = Instant::now();
    let now_unix = crate::utils::now_unix();
    let monitoring = &mut state.monitoring;
    monitoring.poll(now);
    // Statuses change every second; the "Uploading" dot pulses.
    ui.ctx().request_repaint_after(if monitoring.busy() {
        Duration::from_millis(100)
    } else {
        state::STATUS_EVERY
    });
    let mut open_drive = false;
    theme::page_body(ui, "monitoring", |ui| {
        theme::page_header(
            ui,
            tr("Monitoring"),
            Some(tr(
                "Live and past network traffic of every mounted pool, per cloud account.",
            )),
        );
        if monitoring.mounts.is_empty() {
            theme::card_section(
                ui,
                tr("Nothing to monitor"),
                None,
                |_| {},
                |ui| {
                    ui.add(
                    egui::Label::new(tr(
                        "No pool is mounted. Mount a pool on the Drive page to monitor its traffic.",
                    ))
                    .wrap(),
                );
                    ui.add_space(theme::SUBSECTION_GAP);
                    open_drive = theme::primary_button(ui, true, tr("Open Drive")).clicked();
                },
            );
            return;
        }
        for index in 0..monitoring.mounts.len() {
            mount_card(ui, monitoring, index, now, now_unix);
        }
    });
    if open_drive {
        state.page = Page::Drive;
    }
}

/// One mount card: header badges, live summary, Live / History tabs, and the
/// tab's content; stores the chosen tab and range and starts a history load
/// when the History tab is shown.
fn mount_card(
    ui: &mut egui::Ui,
    monitoring: &mut MonitoringState,
    index: usize,
    now: Instant,
    now_unix: u64,
) {
    let mount = &monitoring.mounts[index];
    let view = view_model::mount_view(&mount.entry, mount.status.as_ref(), now_unix);
    let subtitle = view.mountpoint.clone();
    let mut tab = mount.tab;
    let mut range = mount.range;
    ui.push_id(("monitoring-card", &mount.entry.id), |ui| {
        theme::card_section(
            ui,
            &view.pool,
            Some(&subtitle),
            |ui| {
                if let Some(live) = &view.live {
                    live_view::activity_badge(ui, live.activity);
                }
                status_badge(ui, &view.frontend, StatusTone::Neutral);
            },
            |ui| {
                let Some(live) = &view.live else {
                    theme::hint(ui, tr("Waiting for live status from the mount…"));
                    return;
                };
                live_view::summary(ui, live);
                ui.add_space(theme::SUBSECTION_GAP);
                theme::tabs(
                    ui,
                    &mut tab,
                    &[
                        (CardTab::Live, tr("Live")),
                        (CardTab::History, tr("History")),
                    ],
                );
                let mount = &monitoring.mounts[index];
                match tab {
                    CardTab::Live => live_view::remotes(ui, live, &mount.rings, now_unix),
                    CardTab::History => range = history_view::show(ui, range, &mount.history),
                }
            },
        );
    });
    let mount = &mut monitoring.mounts[index];
    mount.tab = tab;
    mount.range = range;
    if tab == CardTab::History {
        monitoring.ensure_history(index, now, now_unix);
    }
}
