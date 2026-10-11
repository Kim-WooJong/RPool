//! Pending revisions reported by the running mount. This is an observation
//! only: a change can be claimed or committed after this snapshot was written.
use super::format;
use crate::gui::i18n::tr;
use crate::gui::theme;
use crate::monitor::model::{QueueOperation, QueueStatus};
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, queue: &QueueStatus, now: u64) -> bool {
    let manage = ui.button(tr("Manage pending uploads…")).clicked();
    let Some(items) = &queue.items else {
        theme::hint(ui, tr("Queue details are unavailable from this mount. Restart it with the updated RPool version."));
        return manage;
    };
    if items.is_empty() {
        theme::hint(ui, tr("No pending drive changes."));
        return manage;
    }
    theme::hint(ui, tr("Each row is a pending change, not necessarily a distinct file. A path may appear more than once."));
    ui.add_space(theme::SUBSECTION_GAP);
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            for item in items {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&item.path).strong());
                    theme::hint(
                        ui,
                        match item.operation {
                            QueueOperation::Write => tr("Upload"),
                            QueueOperation::Delete => tr("Delete"),
                        },
                    );
                    if item.operation == QueueOperation::Write {
                        theme::hint(ui, &format::bytes(item.size));
                    }
                    theme::hint(ui, &format::duration(now.saturating_sub(item.since_unix)));
                });
                ui.separator();
            }
        });
    manage
}
