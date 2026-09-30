use super::state::IntegrityForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::inventory::load_inventory;
use crate::presentation::format_bytes;
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(
    ui: &mut egui::Ui,
    form: &mut IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) {
    ui.label(egui::RichText::new(tr("Scrub")).strong());
    ui.horizontal(|ui| {
        ui.selectable_value(&mut form.quick, true, tr("Quick"));
        ui.selectable_value(&mut form.quick, false, tr("Full BLAKE3"));
    });
    ui.small(if form.quick {
        tr("Checks object existence and expected size.")
    } else {
        tr("Streams every shard and validates BLAKE3.")
    });

    if !form.library_archive_id.is_empty() {
        if let Ok(store) = load_inventory() {
            if let Some(entry) = store.entries.get(&form.library_archive_id) {
                let coding = entry
                    .coding
                    .as_ref()
                    .map(|coding| format!("{}+{}", coding.data_shards, coding.parity_shards))
                    .unwrap_or_else(|| tr("No EC").to_string());
                ui.small(trf(
                    "Target {name} · {size} · coding {coding} · workers {workers}",
                    &[
                        ("name", &entry.original_name),
                        ("size", &format_bytes(entry.original_size)),
                        ("coding", &coding),
                        ("workers", &workers),
                    ],
                ));
            }
        }
    } else if !form.manifest.trim().is_empty() {
        ui.small(trf(
            "Target manifest {manifest} · workers {workers}",
            &[("manifest", &form.manifest.trim()), ("workers", &workers)],
        ));
    }

    let ready = !form.manifest.trim().is_empty() && !task.is_running();
    ui.horizontal(|ui| {
        ui.label(trf(
            "Workers {workers} · Retries {retries}",
            &[("workers", &workers), ("retries", &retries)],
        ));
        if ui
            .add_enabled(ready, egui::Button::new(tr("Run scrub")))
            .clicked()
        {
            form.error = start(form, task, rclone, workers, retries).err();
            form.notice = None;
        }
    });
}

fn start(
    form: &IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) -> Result<(), String> {
    let manifest = form.manifest.trim();
    if manifest.is_empty() {
        return Err(tr("Select or enter a manifest first.").to_string());
    }
    let mut args = vec![
        OsString::from("scrub"),
        OsString::from(manifest),
        OsString::from("--workers"),
        OsString::from(workers.to_string()),
        OsString::from("--retries"),
        OsString::from(retries.to_string()),
    ];
    if form.quick {
        args.push(OsString::from("--quick"));
    }
    task.start_rpool("Scrub", rclone, args)
}
