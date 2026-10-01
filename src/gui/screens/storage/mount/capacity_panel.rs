//! Capacity verdict first ("writable now" or why not), a per-account table,
//! and the detailed notes collapsed.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::mount::capacity::CapacityStatus;
use crate::presentation::format_bytes;
use eframe::egui;

/// The drive's own free-space rule (`VirtualDrive::quota`): a snapshot older
/// than this reports no additional space.
const FRESH_SECONDS: u64 = 120;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Writable(u64),
    NotWritable(Vec<String>),
}

fn age(capacity: &CapacityStatus, now: u64) -> u64 {
    now.saturating_sub(capacity.observed_unix)
}

/// What Explorer/Finder will show, with the reasons when writes are blocked.
pub(super) fn verdict(capacity: &CapacityStatus, now: u64) -> Verdict {
    let age = age(capacity, now);
    if age <= FRESH_SECONDS && !capacity.eligible.is_empty() && capacity.additional_estimate > 0 {
        return Verdict::Writable(capacity.additional_estimate);
    }
    let mut reasons = Vec::new();
    if age > FRESH_SECONDS {
        reasons.push(trf("Measured {age} s ago. The drive reports 0 bytes free until capacity is measured again; this happens automatically while mounted.", &[("age", &age)]));
    }
    if capacity.eligible.is_empty() {
        reasons.push(tr("No account answered a quota query.").into());
    }
    let unanswered: Vec<&str> = capacity
        .excluded
        .iter()
        .map(|e| e.remote.as_str())
        .collect();
    if !unanswered.is_empty() {
        reasons.push(trf(
            "Not usable: {remotes} (see the table).",
            &[("remotes", &unanswered.join(", "))],
        ));
    }
    let undeclared = capacity.targets.iter().filter(|t| !t.declared).count();
    if undeclared > 0 {
        reasons.push(trf("{undeclared} account(s) have no declared identity; declare them in Storage › Pools › Account identities.", &[("undeclared", &undeclared)]));
    }
    if capacity.required_failure_groups > capacity.eligible_failure_groups {
        reasons.push(trf(
            "Resilient placement needs {required} outage groups; {usable} are declared and usable.",
            &[
                ("required", &capacity.required_failure_groups),
                ("usable", &capacity.eligible_failure_groups),
            ],
        ));
    }
    if reasons.is_empty() {
        reasons.push(tr("No file fits the current quotas and placement rules.").into());
    }
    Verdict::NotWritable(reasons)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    let Some(capacity) = &form.session.capacity else {
        theme::card_section(
            ui,
            tr("Capacity"),
            None,
            |_| {},
            |ui| {
                status_badge(ui, tr("Not measured yet"), StatusTone::Info);
                theme::hint(ui, tr("Shown while mounted or after Check capacity. Until then an online drive reports 0 bytes free."));
            },
        );
        return;
    };
    let mut migrate = false;
    theme::card_section(
        ui,
        tr("Capacity"),
        None,
        |_| {},
        |ui| {
            match verdict(capacity, now()) {
                Verdict::Writable(bytes) => {
                    ui.horizontal(|ui| {
                        status_badge(ui, tr("Writable"), StatusTone::Success);
                        ui.label(
                            egui::RichText::new(trf(
                                "{size} can be written now",
                                &[("size", &format_bytes(bytes))],
                            ))
                            .strong(),
                        );
                    });
                }
                Verdict::NotWritable(reasons) => {
                    ui.horizontal(|ui| {
                        status_badge(ui, tr("Not writable yet"), StatusTone::Warning);
                        ui.label(egui::RichText::new(tr("The drive shows 0 bytes free")).strong());
                    });
                    for reason in reasons {
                        ui.label(format!("• {reason}"));
                    }
                }
            }
            ui.small(trf(
                "Known files {size} · measured {age} s ago · {usable} of {total} accounts usable",
                &[
                    ("size", &format_bytes(capacity.logical_used)),
                    ("age", &age(capacity, now())),
                    ("usable", &capacity.eligible.len()),
                    (
                        "total",
                        &(capacity.eligible.len() + capacity.excluded.len()),
                    ),
                ],
            ));
            ui.add_space(theme::SUBSECTION_GAP);
            egui::Grid::new("mount-capacity-accounts")
                .num_columns(4)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.strong(tr("Account"));
                    ui.strong(tr("Free / total"));
                    ui.strong(tr("Identity"));
                    ui.strong(tr("Status"));
                    ui.end_row();
                    for target in &capacity.targets {
                        ui.label(format!("{} › {}", target.remote, target.backing));
                        ui.label(format!(
                            "{} / {}",
                            format_bytes(target.free),
                            format_bytes(target.total)
                        ));
                        ui.label(if target.declared {
                            format!(
                                "{} · {}",
                                target.capacity_domain,
                                target
                                    .failure_domain
                                    .as_deref()
                                    .unwrap_or(tr("no outage group"))
                            )
                        } else {
                            tr("not declared").into()
                        });
                        status_badge(ui, tr("OK"), StatusTone::Success);
                        ui.end_row();
                    }
                    for excluded in &capacity.excluded {
                        ui.label(&excluded.remote);
                        ui.label("—");
                        ui.label("");
                        status_badge(
                            ui,
                            if excluded.temporary {
                                tr("Unavailable")
                            } else {
                                tr("Excluded")
                            },
                            if excluded.temporary {
                                StatusTone::Warning
                            } else {
                                StatusTone::Error
                            },
                        );
                        ui.end_row();
                    }
                });
            for excluded in &capacity.excluded {
                ui.small(format!("{}: {}", excluded.remote, excluded.reason));
            }
            egui::CollapsingHeader::new(tr("Capacity details"))
            .id_salt("mount-capacity-details")
            .show(ui, |ui| {
                crate::gui::widgets::pool_capacity::summary(ui, capacity);
                ui.small(&capacity.note);
                if let Some(committed) = capacity.committed_logical_used {
                    ui.small(trf("Known committed shared namespace: {size} (data only)", &[("size", &format_bytes(committed))]));
                }
                ui.small(tr("Usage excludes parity and may exclude writes still in the OS cache. Retained cloud history uses physical quota."));
                if capacity.retained_archives > 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, trf("{active} active archives and {manifests} known manifests reference excluded accounts.", &[("active", &capacity.affected_active), ("manifests", &capacity.retained_archives)]));
                    ui.small(tr("Migration switches active references only after verified copying; originals are kept. Unmount and drain the OS cache first."));
                    migrate = ui
                        .add_enabled(
                            !form.virtual_drive && !form.session.runner.is_running() && capacity.affected_active > 0 && !capacity.eligible.is_empty(),
                            egui::Button::new(tr("Migrate active archives — keep originals")),
                        )
                        .clicked();
                }
            });
        },
    );
    if migrate {
        super::status_bar::run(form, &mut state.settings, |form, rclone| {
            form.start_action(rclone, 3)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::capacity::Excluded;

    fn measured(now: u64) -> CapacityStatus {
        let mut status = CapacityStatus::default();
        status.eligible = vec!["a:".into()];
        status.additional_estimate = 5 << 30;
        status.observed_unix = now - 10;
        status
    }

    #[test]
    fn a_fresh_usable_measurement_is_writable() {
        assert_eq!(verdict(&measured(1000), 1000), Verdict::Writable(5 << 30));
    }

    #[test]
    fn stale_unanswered_or_blocked_measurements_explain_why() {
        let Verdict::NotWritable(reasons) = verdict(&measured(1000), 1200) else {
            panic!()
        };
        assert!(reasons[0].contains("Measured 210 s ago"), "{reasons:?}");
        let mut cancelled = measured(1000);
        cancelled.eligible.clear();
        cancelled.excluded.push(Excluded {
            remote: "drime_1_crypt:".into(),
            reason: "Quota query: cancelled".into(),
            temporary: true,
        });
        let Verdict::NotWritable(reasons) = verdict(&cancelled, 1000) else {
            panic!()
        };
        assert!(
            reasons.iter().any(|r| r.contains("drime_1_crypt:")),
            "{reasons:?}"
        );
        let mut resilient = measured(1000);
        resilient.additional_estimate = 0;
        resilient.required_failure_groups = 4;
        resilient.eligible_failure_groups = 1;
        let Verdict::NotWritable(reasons) = verdict(&resilient, 1000) else {
            panic!()
        };
        assert!(
            reasons.iter().any(|r| r.contains("needs 4 outage groups")),
            "{reasons:?}"
        );
    }
}
