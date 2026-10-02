//! A pool layout change the running mount keeps for later: started uploads
//! finish with the previous layout, then the next mount applies the new one.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::mount::layout_refresh::Layout;
use eframe::egui;

/// Shard size, K + M and placement of a layout as one line.
fn describe(layout: &Layout) -> String {
    let mib = layout.shard_size as f64 / (1024.0 * 1024.0);
    trf(
        "{size} MiB shards · {data} data + {parity} parity · {placement}",
        &[
            ("size", &format!("{mib:.0}")),
            ("data", &layout.data_shards),
            ("parity", &layout.parity_shards),
            ("placement", &tr(layout.placement.label())),
        ],
    )
}

/// Warning that a pool layout change is deferred until pending uploads
/// finish, with the active and next layouts; hidden when nothing is deferred.
pub(super) fn show(ui: &mut egui::Ui, form: &MountForm) {
    let Some(deferral) = &form.session.layout_status else {
        return;
    };
    ui.add_space(theme::SUBSECTION_GAP);
    let color = theme::warning_colors(ui.visuals().dark_mode).1;
    ui.colored_label(
        color,
        trf(
            "Pool layout change is deferred: {n} pending upload(s) use the previous layout; the new layout applies on the next mount after they finish.",
            &[("n", &deferral.pending)],
        ),
    );
    ui.small(trf(
        "This mount: {active} · next mount: {requested}",
        &[
            ("active", &describe(&deferral.active)),
            ("requested", &describe(&deferral.requested)),
        ],
    ));
}
