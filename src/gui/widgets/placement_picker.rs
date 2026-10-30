//! Placement picker: one method combo box and one "provider outage
//! protection" switch (instead of a long list of method × outage variants).
//! Used by the pool form, reprocess, upload options and Settings.
use crate::gui::i18n::{tr, trf};
use crate::models::{Placement, PlacementMethod};
use eframe::egui;

/// Shows the method picker and the outage switch for `placement`
/// (`id` keeps several pickers apart). Free-space ratio with protection
/// also offers the strict declared-outage-group mode (Resilient).
pub(crate) fn placement_picker(ui: &mut egui::Ui, id: &str, placement: &mut Placement) {
    ui.vertical(|ui| {
        let mut method = placement.method();
        egui::ComboBox::from_id_salt(("placement-method", id))
            .selected_text(tr(method.label()))
            .show_ui(ui, |ui| {
                for option in PlacementMethod::ALL {
                    ui.selectable_value(&mut method, option, tr(option.label()));
                }
            });
        let mut protection = placement.outage_protection();
        ui.checkbox(&mut protection, tr("Provider-outage protection"))
            .on_hover_text(tr("At most M (parity) shards of each coding group per account, so losing one account keeps files recoverable when the pool has enough accounts. Off: placement may put more on one account."));
        let mut strict = *placement == Placement::Resilient;
        if method == PlacementMethod::FreeRatio && protection {
            ui.checkbox(&mut strict, tr("Count by declared outage groups (strict)"))
                .on_hover_text(tr("Resilient placement: counts shards per declared outage group instead of per account and refuses uploads that would break the bound. Every account needs a declared outage group."));
        }
        let next = Placement::from_parts(method, protection, strict);
        if next != *placement {
            *placement = next;
        }
    });
}

/// Warning when protection is on but the pool has too few accounts for it;
/// `None` otherwise.
pub(crate) fn protection_gap_warning(
    placement: Placement,
    data_shards: usize,
    parity_shards: usize,
    accounts: usize,
) -> Option<String> {
    let (have, need) =
        crate::models::outage_protection_gap(placement, data_shards, parity_shards, accounts)?;
    Some(trf(
        "Provider-outage protection is not possible with this setup: {k}+{m} needs at least {need} accounts for at most {m} shards each, the pool has {have}. Losing one account may make files unrecoverable.",
        &[("k", &data_shards), ("m", &parity_shards), ("need", &need), ("have", &have)],
    ))
}
