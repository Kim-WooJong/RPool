//! Capacity preview for unsaved pool options: a read-only provider quota query
//! run off the UI thread, plus a summary of the resulting capacity. Used by the
//! pool editor (`screens::storage::pools`) and the mount capacity panel.

use crate::gui::i18n::{tr, trf};
use crate::models::PoolDefinition;
use crate::mount::capacity::CapacityStatus;
use crate::pool::capacity::PoolCapacity;
use crate::presentation::format_bytes;
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Background capacity query tied to the options it was started for.
#[derive(Debug, Default)]
pub(crate) struct CapacityPreview {
    /// Channel of the running query; `None` when idle.
    pending: Option<Receiver<Result<PoolCapacity, String>>>,
    /// Serialized `(rclone, policy)` of the last query; results for other
    /// options are discarded.
    signature: String,
    /// Last completed report.
    report: Option<PoolCapacity>,
    /// Error of the last query, if it failed.
    error: Option<String>,
}
impl CapacityPreview {
    /// The last completed report for the current options, if any.
    pub(crate) fn report(&self) -> Option<&PoolCapacity> {
        self.report.as_ref()
    }

    /// Starts a background query for `policy` (replacing any running one).
    pub(crate) fn start(&mut self, rclone: &str, policy: &PoolDefinition) {
        self.signature = serde_json::to_string(&(rclone, policy)).unwrap_or_default();
        self.report = None;
        self.error = None;
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        let rclone = rclone.to_owned();
        let policy = policy.clone();
        std::thread::spawn(move || {
            let result =
                crate::pool::capacity::query(&rclone, &policy).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }

    /// Draws the capacity section: collects a finished query, offers a
    /// Calculate/refresh button and shows totals, per-target quotas and
    /// exclusions. Changing the options clears the shown result.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, rclone: &str, policy: &PoolDefinition) {
        let signature = serde_json::to_string(&(rclone, policy)).unwrap_or_default();
        if self.signature != signature {
            self.report = None;
            self.error = None;
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    if self.signature == signature {
                        match result {
                            Ok(report) => self.report = Some(report),
                            Err(e) => self.error = Some(e),
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some(tr("Capacity query stopped; retry.").into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        ui.separator();
        ui.strong(tr("Capacity for these pool options"));
        ui.small(tr(
            "Read-only provider query. No save, workspace or mount required.",
        ));
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new(tr("Calculate / refresh capacity")),
            )
            .clicked()
        {
            self.start(rclone, policy);
        }
        if self.pending.is_some() {
            ui.spinner();
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::YELLOW, e);
        }
        if let Some(report) = &self.report {
            if let Some(note) = report.policy.placement.protection_note() {
                ui.colored_label(egui::Color32::YELLOW, tr(note));
            }
            summary(ui, &report.capacity);
            ui.small(tr("Logical file usage: not queried (requires workspace namespace). Account usage includes history, parity and unrelated files."));
            if !report.capacity.quota_complete
                || (report.capacity.required_failure_groups > 0
                    && report.capacity.resilient_remaining_upper.is_none())
            {
                ui.small(tr("Declare account identities below to combine independent accounts; crypt aliases share their backing account."));
            }
            for target in &report.capacity.targets {
                ui.small(trf(
                    "{remote} › {backing} · quota group {group} · {free} free / {total} total · {identity}",
                    &[
                        ("remote", &target.remote),
                        ("backing", &target.backing),
                        ("group", &target.capacity_domain),
                        ("free", &format_bytes(target.free)),
                        ("total", &format_bytes(target.total)),
                        (
                            "identity",
                            &if target.declared {
                                tr("declared")
                            } else {
                                tr("unverified account identity")
                            },
                        ),
                    ],
                ));
            }
            for excluded in &report.capacity.excluded {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    trf(
                        "Excluded {remote}: {reason}",
                        &[("remote", &excluded.remote), ("reason", &excluded.reason)],
                    ),
                );
            }
            ui.collapsing(tr("Calculation details"), |ui| {
                ui.small(&report.capacity.note);
            });
        }
    }
}
/// Shows the pool data capacity after parity (total and remaining), noting
/// when quotas were partial. Also used by the mount capacity panel.
pub(crate) fn summary(ui: &mut egui::Ui, c: &CapacityStatus) {
    if c.accounts.is_empty() {
        ui.colored_label(
            egui::Color32::YELLOW,
            tr("Account capacity unknown — no usable quota response."),
        );
        return;
    }
    ui.strong(trf(
        "Pool data capacity after parity: {total} total · {remaining} remaining{note}",
        &[
            ("total", &format_bytes(c.nominal_logical_upper)),
            ("remaining", &format_bytes(c.remaining_logical_upper)),
            (
                "note",
                &if c.quota_complete {
                    ""
                } else {
                    tr(" (conservative reported quota; partial)")
                },
            ),
        ],
    ));
    ui.small(tr("Calculated from the sum of independent account quotas × data shards / (data + parity shards), not the smallest provider multiplied as RAID. This is a coding-only upper bound, not guaranteed writable space."));
    ui.label(trf(
        "Account storage: {occupied} occupied / {total} total · {free} free{note}",
        &[
            ("occupied", &format_bytes(c.physical_occupied)),
            ("total", &format_bytes(c.physical_total)),
            ("free", &format_bytes(c.physical_free)),
            (
                "note",
                &if c.quota_complete {
                    ""
                } else {
                    tr(" (partial; not the full pool)")
                },
            ),
        ],
    ));
    ui.small(tr("Account occupied space includes parity, retained history and files outside RPool; it is not the visible RPool file size."));
    if let Some(scenario) = &c.independent_quota_scenario {
        ui.label(trf(
            "If unverified backing accounts are independent: {total} total data · {remaining} remaining data ({physical_total} physical total · {physical_free} physical free)",
            &[
                ("total", &format_bytes(scenario.nominal_logical_upper)),
                ("remaining", &format_bytes(scenario.remaining_logical_upper)),
                ("physical_total", &format_bytes(scenario.physical_total)),
                ("physical_free", &format_bytes(scenario.physical_free)),
            ],
        ));
        ui.small(tr("What-if estimate only. Distinct remote names can share one account; declare quota identities before treating this as available capacity. It does not change upload admission or the mounted OS free-space report."));
    }
    if c.required_failure_groups > 0 {
        ui.label(trf(
            "Resilient outage groups: {eligible} declared and quota-eligible / {required} needed for a full K+M stripe (smaller files may still fit)",
            &[
                ("eligible", &c.eligible_failure_groups),
                ("required", &c.required_failure_groups),
            ],
        ));
        if let Some(upper) = c.resilient_remaining_upper {
            ui.label(trf(
                "Outage-aware remaining upper bound: {upper} (not guaranteed writable)",
                &[("upper", &format_bytes(upper))],
            ));
        }
    }
    if let Some(b) = c
        .balance
        .as_ref()
        .filter(|b| b.groups.iter().any(|g| g.unusable > 0))
    {
        ui.label(trf(
            "Uneven groups: one group holds at most {cap} shards of each coding group, so data after parity fits up to {usable}",
            &[
                ("cap", &b.per_group_cap),
                ("usable", &format_bytes(b.usable_logical)),
            ],
        ));
        for g in b.groups.iter().filter(|g| g.unusable > 0) {
            ui.small(trf(
                "{group}: {unusable} of {free} free cannot be filled",
                &[
                    ("group", &g.group),
                    ("unusable", &format_bytes(g.unusable)),
                    ("free", &format_bytes(g.free)),
                ],
            ));
        }
        if b.add_to_use_all > 0 {
            ui.small(trf(
                "To fill every group, add at least {add} in {count} or more new group(s), each no larger than the largest",
                &[
                    ("add", &format_bytes(b.add_to_use_all)),
                    ("count", &b.add_groups_min),
                ],
            ));
        }
    }
    ui.label(trf(
        "Placement-checked next-file estimate: {estimate}{note}",
        &[
            ("estimate", &format_bytes(c.additional_estimate)),
            (
                "note",
                &if c.estimate_limited {
                    tr(" (simulation-capped lower bound)")
                } else {
                    ""
                },
            ),
        ],
    ));
    if c.additional_estimate == 0 {
        ui.colored_label(egui::Color32::YELLOW, &c.note);
    }
    if c.pending_physical_reservation > 0 {
        ui.small(trf(
            "Pending upload reservation: {size} physical",
            &[("size", &format_bytes(c.pending_physical_reservation))],
        ));
    }
    if !c.quota_complete {
        ui.colored_label(egui::Color32::YELLOW, tr("Partial/conservative result: missing quota or unverified account identities. Not the full pool maximum."));
    }
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(c.observed_unix))
        .unwrap_or(0);
    ui.small(trf("Observed {age}s ago. Upper bounds exclude placement restrictions, small-file padding, metadata/encryption and temporary version copies; they are not guaranteed writable space.", &[("age", &age)]));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_draft_discards_late_capacity_response_without_starting_io() {
        let policy = PoolDefinition {
            max_object_bytes: None,
            remotes: vec!["a:".into()],
            ..Default::default()
        };
        let report = PoolCapacity {
            policy: policy.clone(),
            namespace_used: None,
            capacity: Default::default(),
            backings: vec![],
        };
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(report.clone())).unwrap();
        let mut preview = CapacityPreview {
            pending: Some(rx),
            signature: serde_json::to_string(&("unused-rclone", &policy)).unwrap(),
            ..Default::default()
        };
        let mut edited = policy.clone();
        edited.parity_shards += 1;
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            preview.show(ui, "unused-rclone", &edited);
        })
        .drop_without_applying_deltas();
        assert!(preview.pending.is_none());
        assert!(preview.report.is_none());
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(report)).unwrap();
        preview.pending = Some(rx);
        ctx.run_ui(Default::default(), |ui| {
            preview.show(ui, "unused-rclone", &policy);
        })
        .drop_without_applying_deltas();
        assert!(preview.report.is_some());
    }
}
