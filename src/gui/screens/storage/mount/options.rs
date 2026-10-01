//! Drive › Options: how this PC runs the drive. Cache budgets, filesystem
//! frontend and history, saved per pool on this PC.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

pub(super) fn gib(ui: &mut egui::Ui, label: &str, value: &mut u64, min: u64, hint: &str) {
    ui.label(label).on_hover_text(hint);
    ui.add(
        egui::DragValue::new(value)
            .range(min..=1_048_576)
            .suffix(" GiB"),
    );
    ui.end_row();
}

fn cache(ui: &mut egui::Ui, form: &mut MountForm) {
    theme::card_section(
        ui,
        tr("Cache & pending writes"),
        Some(tr("Local disk budgets. Applied on the next start.")),
        |_| {},
        |ui| {
            egui::Grid::new("mount-cache-grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                if form.virtual_drive {
                    gib(ui, tr("Clean shard cache"), &mut form.cache_gib, 1, tr("Verified cloud data kept locally; least recently used is trimmed first. A whole recovery group must fit."));
                }
                gib(ui, tr("OS cache target"), &mut form.vfs_cache_gib, 1, tr("Cache of the native mount. Open files and unsaved writes may exceed it."));
                gib(ui, tr("Keep disk free"), &mut form.cache_min_free_gib, 0, tr("The OS cache tries to leave this much free disk space."));
                if form.virtual_drive {
                    gib(ui, tr("Pending write limit"), &mut form.spool_gib, 1, tr("Local space for saves not yet uploaded; new writes fail safely at the limit."));
                }
                ui.label(tr("Background interval")).on_hover_text(tr("How often sync runs. Capacity is measured at least every 60 s regardless."));
                ui.add(egui::DragValue::new(&mut form.interval_seconds).range(2..=86400).suffix(" s"));
                ui.end_row();
            });
            theme::hint(
                ui,
                &if form.virtual_drive {
                    trf(
                "Clean caches up to {cache} GiB plus pending writes up to {pending} GiB (not preallocated).",
                &[
                    ("cache", &form.cache_gib.saturating_add(form.vfs_cache_gib)),
                    ("pending", &form.spool_gib),
                ],
            )
                } else {
                    tr("A replica keeps the complete copy; the OS cache target does not limit it.")
                        .into()
                },
            );
        },
    );
}

fn frontend(ui: &mut egui::Ui, form: &mut MountForm) {
    use crate::cli::Frontend;
    let name = |f: Frontend| match f {
        Frontend::Auto => tr("Automatic — native where available (default)"),
        Frontend::Dav => tr("WebDAV via rclone mount"),
        Frontend::Fuse => tr("Native FUSE (Linux)"),
        Frontend::Winfsp => tr("Native WinFsp (Windows)"),
    };
    theme::card_section(
        ui,
        tr("Filesystem frontend"),
        Some(tr("How the drive is served to the operating system.")),
        |_| {},
        |ui| {
            egui::ComboBox::from_id_salt("mount-frontend")
                .width(ui.available_width().min(340.0))
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
            if Frontend::native_here().is_none() {
                match Frontend::native_built() {
                    Some(Frontend::Winfsp) => {
                        theme::hint(ui, tr("WinFsp not installed — install WinFsp for the native drive (then restart RPool); WebDAV is used meanwhile."));
                        ui.hyperlink_to(tr("Download WinFsp"), "https://winfsp.dev/rel/");
                    }
                    Some(_) => theme::hint(
                        ui,
                        tr("The native frontend is unavailable on this PC; WebDAV is used."),
                    ),
                    None if cfg!(windows) => theme::hint(
                        ui,
                        tr("This build was made without WinFsp support; WebDAV is used."),
                    ),
                    None => {
                        theme::hint(ui, tr("No native frontend on this OS yet; WebDAV is used."))
                    }
                }
            } else if !form.native_allowed() {
                theme::hint(ui, tr("Native frontends serve online drives with This PC only or Automatic pool sync. WebDAV is used for shared-root and bounded shared modes."));
            } else if form.native_selected() {
                ui.checkbox(&mut form.native_read_only, tr("Mount read-only"));
                theme::hint(ui, tr("Close and fsync are the local durability points; cloud sync stays asynchronous."));
            }
        },
    );
}

fn history(ui: &mut egui::Ui, form: &mut MountForm) {
    theme::card_section(
        ui,
        tr("Version history (pool sync)"),
        Some(tr(
            "How many previous versions each file keeps. Every PC must use the same limit.",
        )),
        |_| {},
        |ui| {
            ui.checkbox(&mut form.pool_retention, tr("Automatic history deletion (v7)"))
            .on_hover_text(tr("Keeps current files, the chosen number of previous versions and unresolved conflicts."));
            if !form.pool_retention {
                ui.checkbox(
                    &mut form.pool_history_override,
                    tr("Save a future history limit in the workspace"),
                );
            }
            if form.pool_history_override || form.pool_retention {
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("Previous versions per file"));
                    ui.add(egui::DragValue::new(&mut form.pool_history_limit).range(0..=10000));
                });
            }
            if form.pool_retention {
                ui.checkbox(&mut form.diagnostic_read_only, tr("Next mount: diagnostic read-only"))
                .on_hover_text(tr("Mounts an existing v7 pool read-only for inspection: no background sync, upload or history collection. Not saved; applies to the next Mount only."));
            }
            theme::hint(
                ui,
                if form.pool_retention {
                    tr("History collection needs temporary space. Keep the saved mode for an existing workspace.")
                } else {
                    tr("Without v7 the limit is stored only; nothing is deleted automatically.")
                },
            );
        },
    );
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    ui.add_enabled_ui(!form.session.runner.is_running(), |ui| {
        let virtual_drive = form.virtual_drive;
        let pool_sync = form.virtual_drive && form.pool_sync;
        theme::two_up(ui, form, cache, |ui, form| {
            if virtual_drive {
                frontend(ui, form);
            }
            if pool_sync {
                history(ui, form);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, true, tr("Save settings"))
                .on_hover_text(tr("Saved per pool on this PC; also saved on every start."))
                .clicked()
            {
                form.session.notice = Some(match form.save_mount_settings(settings) {
                    Ok(()) => tr("Mount settings saved for this pool on this PC.").into(),
                    Err(error) => error,
                });
            }
            if let Some(notice) = &form.session.notice {
                ui.label(notice);
            }
        });
    });
}
