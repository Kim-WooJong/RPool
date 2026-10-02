//! Drive › Import: bring existing data into this drive. RPool archives by
//! manifest (applied at the next start), or any rclone path (offline copy).
use super::form::MountForm;
use super::options::gib;
use super::status_bar::run;
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

/// "RPool archives" card: add or remove manifests applied at the next start.
fn manifests(ui: &mut egui::Ui, form: &mut MountForm) {
    theme::card_section(ui, tr("RPool archives"), Some(tr("Adds archives made with Upload to the drive at the next Mount or Sync. Pool membership is never inferred.")), |_| {}, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.manifest_input)
                    .desired_width(ui.available_width().min(320.0))
                    .hint_text(tr("manifest path or remote")),
            );
            if ui.button(tr("Add")).clicked() && !form.manifest_input.trim().is_empty() {
                let value = form.manifest_input.trim().to_string();
                if !form.manifests.contains(&value) {
                    form.manifests.push(value);
                }
                form.manifest_input.clear();
            }
            if ui.button(tr("Browse…")).clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Manifest JSON", &["json"])
                    .pick_files()
                {
                    for path in paths {
                        let value = path.display().to_string();
                        if !form.manifests.contains(&value) {
                            form.manifests.push(value);
                        }
                    }
                }
            }
        });
        if form.manifests.is_empty() {
            theme::hint(ui, tr("No archives listed."));
        }
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt("mount-manifests").max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
            for (index, source) in form.manifests.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button(tr("Remove")).clicked() {
                        remove = Some(index);
                    }
                    ui.label(source);
                });
            }
        });
        if let Some(index) = remove {
            form.manifests.remove(index);
        }
    });
}

/// "Files stored with rclone" card: source, destination, batch size and the
/// Import button (mount action 9), plus the running import's progress.
fn rclone(
    ui: &mut egui::Ui,
    form: &mut MountForm,
    settings: &mut crate::gui::settings::GuiSettings,
) {
    theme::card_section(ui, tr("Files stored with rclone"), Some(tr("Copies any rclone path (for example an old crypt remote) into this drive and uploads it as pool shards. Unmount first. The source is only read.")), |_| {}, |ui| {
        egui::Grid::new("rclone-import-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            let width = (ui.available_width() - 120.0).clamp(160.0, 320.0);
            ui.label(tr("Source"));
            ui.add(egui::TextEdit::singleline(&mut form.import_source).desired_width(width).hint_text("old-crypt:photos"));
            ui.end_row();
            ui.label(tr("Drive folder"));
            ui.add(egui::TextEdit::singleline(&mut form.import_destination).desired_width(width).hint_text(tr("(drive root)")));
            ui.end_row();
            gib(ui, tr("Upload every"), &mut form.import_batch_gib, 1, tr("Uploads after this much was copied, so local disk only holds one batch."));
        });
        ui.checkbox(&mut form.import_rename, tr("Import name clashes as \"name (imported N)\""))
            .on_hover_text(tr("Off: a file that already exists in the drive is skipped and listed."));
        if theme::primary_button(ui, !form.session.runner.is_running(), tr("Import")).clicked() {
            run(form, settings, |form, rclone| form.start_action(rclone, 9));
        }
        if let Some(status) = &form.session.import_status {
            ui.label(trf("{phase}: {done}/{total} files · {imported} imported · {skipped} skipped · {failed} failed", &[("phase", &status.phase), ("done", &status.files_done), ("total", &status.files_total), ("imported", &status.imported), ("skipped", &status.skipped.len()), ("failed", &status.failed.len())]));
            egui::ScrollArea::vertical().id_salt("import-issues").max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                for (path, reason) in status.skipped.iter().chain(&status.failed) {
                    ui.small(format!("{path}: {reason}"));
                }
            });
        }
        theme::hint(ui, tr("Modification times are not kept. Names the drive cannot store are skipped and listed. Many small files upload slowly (each becomes its own set of shards)."));
    });
}

/// Import tab of the Drive page; disabled while the session runs.
pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    ui.add_enabled_ui(!form.session.runner.is_running(), |ui| {
        theme::two_up(ui, form, |ui, form| rclone(ui, form, settings), manifests);
    });
}
