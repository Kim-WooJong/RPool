use crate::models::PoolDefinition;
use crate::mount::capacity::CapacityStatus;
use crate::pool::capacity::PoolCapacity;
use crate::presentation::format_bytes;
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Debug, Default)]
pub(crate) struct CapacityPreview {
    pending: Option<Receiver<Result<PoolCapacity, String>>>,
    signature: String,
    report: Option<PoolCapacity>,
    error: Option<String>,
}
impl CapacityPreview {
    pub(crate) fn invalidate(&mut self) {
        self.pending = None;
        self.signature.clear();
        self.report = None;
        self.error = None;
    }

    /// Returns true when the user wants to open the account identity editor.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        rclone: &str,
        policy: &PoolDefinition,
    ) -> bool {
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
                    self.error = Some("Capacity query stopped; retry.".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        ui.separator();
        ui.strong("Capacity for these pool options");
        ui.small("Read-only provider query. No save, workspace or mount required.");
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new("Calculate / refresh capacity"),
            )
            .clicked()
        {
            self.signature = signature;
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
                ui.colored_label(egui::Color32::YELLOW, note);
            }
            summary(ui, &report.capacity);
            ui.small("Logical file usage: not queried (requires workspace namespace). Account usage includes history, parity and unrelated files.");
            if !report.capacity.quota_complete
                || (report.capacity.required_failure_groups > 0
                    && report.capacity.resilient_remaining_upper.is_none())
            {
                ui.small("Independent account budgets and outage groups must be declared explicitly; crypt aliases share their backing account.");
                if ui
                    .button("Set account / outage groups in Mount drive")
                    .clicked()
                {
                    return true;
                }
            }
            for target in &report.capacity.targets {
                ui.small(format!(
                    "{} → {} · quota group {} · {} free / {} total · {}",
                    target.remote,
                    target.backing,
                    target.capacity_domain,
                    format_bytes(target.free),
                    format_bytes(target.total),
                    if target.declared {
                        "declared"
                    } else {
                        "unverified account identity"
                    }
                ));
            }
            for excluded in &report.capacity.excluded {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("Excluded {}: {}", excluded.remote, excluded.reason),
                );
            }
            ui.collapsing("Calculation details", |ui| {
                ui.small(&report.capacity.note);
            });
        }
        false
    }
}
pub(crate) fn summary(ui: &mut egui::Ui, c: &CapacityStatus) {
    if c.accounts.is_empty() {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Account capacity unknown — no usable quota response.",
        );
        return;
    }
    ui.label(format!(
        "Known account budget: {} occupied / {} total · {} free{}",
        format_bytes(c.physical_occupied),
        format_bytes(c.physical_total),
        format_bytes(c.physical_free),
        if c.quota_complete {
            ""
        } else {
            " (partial; not the full pool)"
        }
    ));
    ui.label(format!(
        "Coding-only maximum upper bound: {} · remaining upper bound: {}",
        format_bytes(c.nominal_logical_upper),
        format_bytes(c.remaining_logical_upper)
    ));
    if c.required_failure_groups > 0 {
        ui.label(format!(
            "Resilient outage groups: {} declared and quota-eligible / {} minimum for this K+M layout",
            c.eligible_failure_groups, c.required_failure_groups
        ));
        if let Some(upper) = c.resilient_remaining_upper {
            ui.label(format!(
                "Outage-aware remaining upper bound: {} (not guaranteed writable)",
                format_bytes(upper)
            ));
        }
    }
    ui.label(format!(
        "Placement-verified additional file estimate: {}{}",
        format_bytes(c.additional_estimate),
        if c.estimate_limited {
            " (simulation-capped lower bound)"
        } else {
            ""
        }
    ));
    if c.additional_estimate == 0 {
        ui.colored_label(egui::Color32::YELLOW, &c.note);
    }
    if c.pending_physical_reservation > 0 {
        ui.small(format!(
            "Pending upload reservation: {} physical",
            format_bytes(c.pending_physical_reservation)
        ));
    }
    if !c.quota_complete {
        ui.colored_label(egui::Color32::YELLOW, "Partial/conservative result: missing quota or unverified account identities. Not the full pool maximum.");
    }
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(c.observed_unix))
        .unwrap_or(0);
    ui.small(format!("Observed {age}s ago. Upper bounds exclude placement restrictions, small-file padding, metadata/encryption and temporary version copies; they are not guaranteed writable space."));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_draft_discards_late_capacity_response_without_starting_io() {
        let policy = PoolDefinition {
            remotes: vec!["a:".into()],
            ..Default::default()
        };
        let report = PoolCapacity {
            policy: policy.clone(),
            namespace_used: None,
            capacity: Default::default(),
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

    #[test]
    fn identity_save_invalidation_discards_pending_quota_response() {
        let policy = PoolDefinition::default();
        let report = PoolCapacity {
            policy: policy.clone(),
            namespace_used: None,
            capacity: Default::default(),
        };
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(report)).unwrap();
        let mut preview = CapacityPreview {
            pending: Some(rx),
            signature: serde_json::to_string(&("rclone", &policy)).unwrap(),
            ..Default::default()
        };
        preview.invalidate();
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            preview.show(ui, "rclone", &policy);
        })
        .drop_without_applying_deltas();
        assert!(preview.report.is_none());
        assert!(preview.pending.is_none());
    }
}
