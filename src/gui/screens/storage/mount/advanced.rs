//! Everything a normal mount does not need: cache budgets, frontend, history,
//! imports, maintenance, pool transitions and account recovery. Collapsed.
use super::drive_section::directory_field;
use super::form::MountForm;
use super::status_bar::run;
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

fn gib(ui: &mut egui::Ui, label: &str, value: &mut u64, min: u64, hint: &str) {
    ui.label(label).on_hover_text(hint);
    ui.add(
        egui::DragValue::new(value)
            .range(min..=1_048_576)
            .suffix(" GiB"),
    );
    ui.end_row();
}

fn cache(ui: &mut egui::Ui, form: &mut MountForm) {
    egui::Grid::new("mount-cache-grid")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            if form.virtual_drive {
                gib(ui, "Clean shard cache", &mut form.cache_gib, 1, "Verified cloud data kept locally; least recently used is trimmed first. A whole recovery group must fit.");
            }
            gib(ui, "OS cache target", &mut form.vfs_cache_gib, 1, "Cache of the native mount. Open files and unsaved writes may exceed it.");
            gib(ui, "Keep disk free", &mut form.cache_min_free_gib, 0, "The OS cache tries to leave this much free disk space.");
            if form.virtual_drive {
                gib(ui, "Pending write limit", &mut form.spool_gib, 1, "Local space for saves not yet uploaded; new writes fail safely at the limit.");
            }
            ui.label("Background interval").on_hover_text("How often sync runs. Capacity is measured at least every 60 s regardless.");
            ui.add(egui::DragValue::new(&mut form.interval_seconds).range(2..=86400).suffix(" s"));
            ui.end_row();
        });
    ui.small(if form.virtual_drive {
        format!(
            "Clean caches up to {} GiB plus pending writes up to {} GiB (not preallocated). Settings apply on the next start.",
            form.cache_gib.saturating_add(form.vfs_cache_gib),
            form.spool_gib
        )
    } else {
        "A replica keeps the complete copy; the OS cache target does not limit it.".into()
    });
}

fn frontend(ui: &mut egui::Ui, form: &mut MountForm) {
    use crate::cli::Frontend;
    let name = |f: Frontend| match f {
        Frontend::Auto => "Automatic — native where available (default)",
        Frontend::Dav => "WebDAV via rclone mount",
        Frontend::Fuse => "Native FUSE (Linux)",
        Frontend::Winfsp => "Native WinFsp (Windows)",
    };
    ui.horizontal(|ui| {
        ui.label("Filesystem frontend");
        egui::ComboBox::from_id_salt("mount-frontend")
            .selected_text(name(form.frontend))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut form.frontend, Frontend::Auto, name(Frontend::Auto));
                ui.selectable_value(&mut form.frontend, Frontend::Dav, name(Frontend::Dav));
                if let Some(native) = Frontend::native_here() {
                    ui.add_enabled_ui(form.native_allowed(), |ui| {
                        ui.selectable_value(&mut form.frontend, native, name(native));
                    });
                }
            });
    });
    if Frontend::native_here().is_none() {
        ui.small(if cfg!(windows) {
            "This build has no native frontend (needs the `winfsp` feature and WinFsp); WebDAV is used."
        } else {
            "No native frontend on this OS yet; WebDAV is used."
        });
    } else if !form.native_allowed() {
        ui.small("Native frontends serve online drives with This PC only or Automatic pool sync (without v7 history deletion). WebDAV is used here.");
    } else if form.native_selected() {
        ui.checkbox(&mut form.native_read_only, "Mount read-only");
        ui.colored_label(ui.visuals().warn_fg_color, "Experimental: close/fsync is the local durability point; cloud sync stays asynchronous. Test with a NEW workspace first.");
    }
}

fn history(ui: &mut egui::Ui, form: &mut MountForm) {
    ui.checkbox(&mut form.pool_retention, "Automatic history deletion (v7)")
        .on_hover_text("Keeps current files, the chosen number of previous versions and unresolved conflicts. Every PC must use the same limit.");
    if !form.pool_retention {
        ui.checkbox(
            &mut form.pool_history_override,
            "Save a future history limit in the workspace",
        );
    }
    if form.pool_history_override || form.pool_retention {
        ui.horizontal(|ui| {
            ui.label("Previous versions per file");
            ui.add(egui::DragValue::new(&mut form.pool_history_limit).range(0..=10000));
        });
    }
    if form.pool_retention {
        ui.checkbox(&mut form.diagnostic_read_only, "Next mount: diagnostic read-only")
            .on_hover_text("Mounts an existing v7 pool read-only for inspection: no background sync, upload or history collection. Not saved; applies to the next Mount only.");
    }
    ui.small(if form.pool_retention {
        "History collection needs temporary space. Keep the saved mode for an existing workspace."
    } else {
        "Without v7 the limit is stored only; nothing is deleted automatically."
    });
}

/// Preview → confirm → delete, for local online drives only.
fn retention(
    ui: &mut egui::Ui,
    form: &mut MountForm,
    settings: &mut crate::gui::settings::GuiSettings,
) {
    ui.label("Removes tracked obsolete versions from the cloud for this workspace. Current files, pending writes and unresolved conflicts are kept.");
    ui.horizontal(|ui| {
        ui.label("Keep previous versions per file");
        ui.add(egui::DragValue::new(&mut form.keep_previous).range(0..=10000));
    });
    if ui
        .button("1. Preview obsolete versions")
        .on_hover_text("Lists what would be removed. Nothing is uploaded or deleted.")
        .clicked()
    {
        run(form, settings, |form, rclone| form.start_action(rclone, 7));
    }
    let previewed = form.retention_previewed.as_ref() == Some(&form.retention_key());
    ui.add_enabled_ui(previewed, |ui| {
        ui.checkbox(
            &mut form.retention_confirmed,
            "2. These archives belong only to this workspace (no other PC or pool uses them)",
        );
    });
    if !previewed {
        ui.small("Run the preview for the current limit first; changing the limit, pool or workspace needs a new preview.");
    }
    let delete = egui::Button::new(
        egui::RichText::new("3. Delete obsolete versions").color(ui.visuals().error_fg_color),
    );
    if ui
        .add_enabled(form.retention_ready(), delete)
        .on_hover_text("Deletes exact remote objects. Resumable; provider trash or versioning may delay quota recovery.")
        .clicked()
    {
        run(form, settings, |form, rclone| form.start_action(rclone, 8));
    }
}

fn imports(ui: &mut egui::Ui, form: &mut MountForm) {
    ui.label(
        "Archives are imported only from manifests listed here; pool membership is never inferred.",
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.manifest_input)
                .desired_width(320.0)
                .hint_text("manifest path or remote"),
        );
        if ui.button("Add").clicked() && !form.manifest_input.trim().is_empty() {
            let value = form.manifest_input.trim().to_string();
            if !form.manifests.contains(&value) {
                form.manifests.push(value);
            }
            form.manifest_input.clear();
        }
        if ui.button("Browse…").clicked() {
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
    let mut remove = None;
    for (index, source) in form.manifests.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(source);
            if ui.small_button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        form.manifests.remove(index);
    }
}

fn reprocess_plan(ui: &mut egui::Ui, form: &mut MountForm) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.recovery_reprocess_plan)
                .desired_width(320.0)
                .hint_text("optional completed plan.json"),
        );
        if ui.button("Choose plan…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Reprocess plan", &["json"])
                .pick_file()
            {
                form.recovery_reprocess_plan = path.display().to_string();
            }
        }
    });
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    egui::CollapsingHeader::new("Advanced")
        .id_salt("mount-advanced")
        .show(ui, |ui| {
            ui.add_enabled_ui(!form.runner.is_running(), |ui| {
                ui.collapsing("Cache, pending writes and interval", |ui| cache(ui, form));
                if form.virtual_drive {
                    ui.collapsing("Filesystem frontend", |ui| frontend(ui, form));
                }
                if form.virtual_drive && form.pool_sync {
                    ui.collapsing("History", |ui| history(ui, form));
                }
                if form.retention_allowed() {
                    ui.collapsing("History cleanup", |ui| retention(ui, form, settings));
                }
                ui.collapsing("Import existing archives", |ui| imports(ui, form));
                ui.collapsing("Maintenance", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Save settings").on_hover_text("Saved per pool on this PC; also saved on every start.").clicked() {
                            form.notice = Some(match form.save_mount_settings(settings) {
                                Ok(()) => "Mount settings saved for this pool on this PC.".into(),
                                Err(error) => error,
                            });
                        }
                        if form.virtual_drive {
                            if ui.button("Trim clean cache").on_hover_text("Removes verified clean cache only; pending writes and history stay.").clicked() {
                                run(form, settings, |form, rclone| form.start_action(rclone, 4));
                            }
                            if ui.button("Export recoverable spool").on_hover_text("Unmounted only: copies pending local writes out for recovery.").clicked() {
                                run(form, settings, |form, rclone| form.start_action(rclone, 5));
                            }
                        }
                    });
                });
                if form.virtual_drive && form.pool_sync {
                    ui.collapsing("Apply changed pool to this workspace", |ui| {
                        ui.label("After adding or removing storage accounts: unmount, then apply. The pool name and workspace path stay the same; current files, conflicts and sealed writes are verified in a new metadata generation first.");
                        ui.small("Old history stays in a sibling backup. Other PCs keep the old generation until they apply too. This can take time and space.");
                        reprocess_plan(ui, form);
                        if ui.button("Apply pool changes").clicked() {
                            run(form, settings, |form, rclone| form.start_action(rclone, 6));
                        }
                    });
                }
                ui.collapsing("Recover after removing an account", |ui| {
                    ui.label("1. In Pools, save the remaining accounts as a NEW pool, select it above with a NEW empty workspace, and turn history deletion off.");
                    ui.label("2. Choose the original workspace (unmount it first). Its data is kept; verified contents are copied and uploaded.");
                    directory_field(ui, &mut form.recovery_source, "original workspace");
                    ui.label("Unavailable remote aliases to skip, one per line (no colon or path):");
                    ui.add(egui::TextEdit::multiline(&mut form.recovery_skip_remotes).desired_rows(2).desired_width(320.0));
                    ui.label("Completed Reprocess plan (optional, reuses verified replacement archives):");
                    reprocess_plan(ui, form);
                    if ui.button("Recover files into the selected pool").clicked() {
                        let result = form
                            .save_mount_settings(settings)
                            .and_then(|()| form.start_account_recovery(&settings.rclone));
                        if let Err(error) = result {
                            form.notice = Some(error);
                        }
                    }
                    ui.small("3. Check the recovery report, then mount the new pool normally. Only locally known files, sealed writes and conflicts can be recovered; missing data is reported per file.");
                });
            });
        });
    ui.add_space(theme::SUBSECTION_GAP);
}
