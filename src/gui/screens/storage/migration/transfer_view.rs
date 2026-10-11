//! Per-storage traffic of the current migration CLI run, including idle targets.
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::monitoring::format_rate;
use crate::gui::state::GuiState;
use crate::gui::task::{TaskInfo, TaskRunner};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::progress::StorageTraffic;
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet};

/// Finished task for this migration, avoiding stale totals from another run.
pub(super) fn last_task<'a>(task: &'a TaskRunner, name: &str, id: &str) -> Option<&'a TaskInfo> {
    task.last_task().filter(|finished| {
        finished.name == name
            && finished.invocation.as_ref().is_some_and(|invocation| {
                invocation
                    .args
                    .windows(2)
                    .any(|pair| pair[0] == "--id" && pair[1] == id)
            })
    })
}

pub(super) fn show(ui: &mut egui::Ui, state: &GuiState, traffic: &[StorageTraffic]) {
    let mut names = BTreeSet::new();
    let mut target = BTreeSet::new();
    if let Some(plan) = &state.migration.plan {
        if state.migration.active_id.as_deref() == Some(plan.migration_id.as_str()) {
            target.extend(plan.target.remotes.iter().cloned());
        }
    }
    if target.is_empty() {
        if let Some(pool) = state.pool_definitions.get(&state.migration.pool) {
            target.extend(pool.remotes.iter().cloned());
        }
    }
    names.extend(target.iter().cloned());
    names.extend(traffic.iter().map(|r| r.remote.clone()));
    let current: BTreeMap<_, _> = traffic.iter().map(|r| (r.remote.as_str(), r)).collect();

    egui::CollapsingHeader::new(tr("Transfer by storage")).default_open(true).show(ui, |ui| {
        theme::hint(ui, tr("Traffic on this PC during this run. Rates are 10-second averages; provider-side copies do not use this PC's bandwidth."));
        theme::hint(ui, tr("Totals include metadata and verification reads."));
        theme::hint(ui, tr("Copy transfer bytes appear after each shard finishes."));
        if names.is_empty() {
            theme::hint(ui, tr("Waiting for storage traffic…"));
            return;
        }
        let height = (names.len() as f32 * 62.0 + 12.0).min(280.0);
        theme::fixed_pane_wide(ui, "migration-traffic", egui::vec2(ui.available_width(), height), |ui| {
            for name in &names {
                let value = current.get(name.as_str()).copied();
                let sent = value.map_or(0, |r| r.sent_bytes);
                let received = value.map_or(0, |r| r.received_bytes);
                let up = value.map_or(0.0, |r| r.upload_rate_10s);
                let down = value.map_or(0.0, |r| r.download_rate_10s);
                ui.horizontal_wrapped(|ui| {
                    ui.strong(name);
                    if !target.contains(name) {
                        status_badge(ui, tr("Not in the new layout"), StatusTone::Neutral);
                    }
                    let active_up = value.is_some_and(|r| r.active_uploads > 0 || up > 0.0);
                    let active_down = value.is_some_and(|r| r.active_downloads > 0 || down > 0.0);
                    if active_up { status_badge(ui, tr("Uploading"), StatusTone::Success); }
                    if active_down { status_badge(ui, tr("Downloading"), StatusTone::Info); }
                    if !active_up && !active_down { status_badge(ui, tr("Idle"), StatusTone::Neutral); }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(trf("↑ {rate} · {bytes} uploaded", &[("rate", &format_rate(up)), ("bytes", &crate::presentation::format_bytes(sent))]));
                    ui.label(trf("↓ {rate} · {bytes} downloaded", &[("rate", &format_rate(down)), ("bytes", &crate::presentation::format_bytes(received))]));
                    if let Some(r) = value {
                        if r.server_side_bytes > 0 {
                            ui.label(trf("{bytes} copied in cloud", &[("bytes", &crate::presentation::format_bytes(r.server_side_bytes))]));
                        }
                        if r.failed_ops > 0 || r.retries > 0 {
                            status_badge(ui, &trf("{failed} failed · {retries} retries", &[("failed", &r.failed_ops), ("retries", &r.retries)]), StatusTone::Warning);
                        }
                        if r.last_upload_storage_full {
                            status_badge(ui, tr("Storage full"), StatusTone::Error);
                        } else if r.capacity_no_space {
                            status_badge(ui, tr("No upload space"), StatusTone::Error);
                        }
                    }
                });
                if let Some(error) = value.and_then(|r| r.last_upload_error.as_deref()) {
                    ui.label(egui::RichText::new(error).small().color(theme::pal(ui).danger));
                }
                ui.separator();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PoolDefinition;

    #[test]
    fn many_storage_rows_fit_a_small_migration_window() {
        let pool = PoolDefinition {
            remotes: (0..12)
                .map(|i| format!("long-account-name-{i:02}-crypt"))
                .collect(),
            ..Default::default()
        };
        let mut state = GuiState::new(
            Default::default(),
            Default::default(),
            BTreeMap::from([("family".into(), pool)]),
        );
        state.migration.pool = "family".into();
        let traffic: Vec<_> = (0..12)
            .map(|i| StorageTraffic {
                remote: format!("long-account-name-{i:02}-crypt"),
                sent_bytes: 1 << 30,
                received_bytes: 2 << 30,
                server_side_bytes: 0,
                upload_rate_10s: 1_000_000.0,
                download_rate_10s: 2_000_000.0,
                active_uploads: 1,
                active_downloads: 1,
                failed_ops: 0,
                retries: 0,
                last_upload_error: None,
                last_upload_storage_full: false,
                capacity_no_space: false,
            })
            .collect();
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            theme::apply(&ctx);
            for _ in 0..2 {
                let size = egui::vec2(440.0, 650.0);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                let mut right = 0.0f32;
                let mut output = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        show(ui, &state, &traffic);
                        right = ui.min_rect().right();
                    });
                });
                output.textures_delta.clear();
                assert!(right <= size.x + 1.0, "traffic panel overflowed: {right}");
            }
        }
    }
}
