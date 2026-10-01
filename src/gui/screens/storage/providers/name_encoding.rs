//! Name encoding of crypt remotes: the choices with a short explanation, and
//! the dialog that switches an existing crypt remote (`provider name-encoding`).
use crate::config_sync::provision::FILENAME_ENCODINGS;
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

/// Label of an encoding in a combo box.
pub(crate) fn label(encoding: &str) -> &'static str {
    match encoding {
        "base32768" => tr("base32768 — short names for path-limited storage"),
        "base64" => tr("base64 — shorter names, case-sensitive storage only"),
        _ => tr("base32 — rclone default, works everywhere (recommended)"),
    }
}

/// One-line explanation shown under the combo box.
pub(crate) fn note(encoding: &str) -> &'static str {
    match encoding {
        "base32768" => tr("Names stay encrypted but take about a quarter of the characters. Use it when uploads fail on long paths, e.g. a Windows SFTP server (260-character limit) or OneDrive."),
        "base64" => tr("Names stay encrypted and are about 20% shorter than base32, but upper and lower case differ: never use it on Windows, macOS or other case-insensitive storage."),
        _ => tr("Names stay encrypted using only lower-case letters and digits, which every storage accepts. Encrypted names are about 1.6 times longer than the original."),
    }
}

/// Encoding combo box bound to an index into [`FILENAME_ENCODINGS`].
pub(crate) fn combo(ui: &mut egui::Ui, id: &str, index: &mut usize) {
    let current = FILENAME_ENCODINGS[(*index).min(FILENAME_ENCODINGS.len() - 1)];
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(current))
        .width(ui.available_width().min(380.0))
        .show_ui(ui, |ui| {
            for (i, encoding) in FILENAME_ENCODINGS.iter().enumerate() {
                ui.selectable_value(index, i, label(encoding));
            }
        });
    ui.small(note(
        FILENAME_ENCODINGS[(*index).min(FILENAME_ENCODINGS.len() - 1)],
    ));
}

pub(crate) fn index_of(encoding: &str) -> usize {
    FILENAME_ENCODINGS
        .iter()
        .position(|e| *e == encoding)
        .unwrap_or(0)
}

/// The "Name encoding…" dialog of a provider card.
#[derive(Debug, Default)]
pub(crate) struct Editor {
    pub(crate) open: bool,
    /// Crypt remotes of the provider the dialog was opened for.
    crypts: Vec<String>,
    crypt: String,
    encoding: usize,
    existing_files_ok: bool,
    notice: Option<String>,
}

impl Editor {
    pub(crate) fn open_for(crypts: &[String]) -> Self {
        let crypt = crypts.first().cloned().unwrap_or_default();
        Self {
            open: true,
            crypts: crypts.to_vec(),
            crypt,
            // Most often opened to fix long paths.
            encoding: index_of("base32768"),
            ..Self::default()
        }
    }
}

/// `provider name-encoding` arguments for the dialog's choice.
pub(crate) fn args(crypt: &str, encoding: &str, existing_files_ok: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "provider".into(),
        "name-encoding".into(),
        format!("--remote={}", crypt.trim_end_matches(':')).into(),
        format!("--encoding={encoding}").into(),
    ];
    if existing_files_ok {
        args.push("--existing-files-ok".into());
    }
    args
}

pub(crate) fn show(ctx: &egui::Context, editor: &mut Editor, rclone: &str, task: &mut TaskRunner) {
    if !editor.open {
        return;
    }
    let mut open = true;
    egui::Window::new(tr("Name encoding"))
        .id(egui::Id::new("provider-name-encoding"))
        .open(&mut open)
        .resizable(true)
        .default_width(500.0)
        .show(ctx, |ui| {
            ui.label(tr("Changes how an existing encrypted provider stores file and folder names. Keys, contents and the folder stay the same."));
            ui.add_space(4.0);
            egui::ComboBox::from_id_salt("name-encoding-crypt")
                .selected_text(&editor.crypt)
                .show_ui(ui, |ui| {
                    for crypt in &editor.crypts {
                        ui.selectable_value(&mut editor.crypt, crypt.clone(), crypt.as_str());
                    }
                });
            combo(ui, "name-encoding-choice", &mut editor.encoding);
            ui.separator();
            ui.label(tr("Files already stored with the old encoding are not deleted, but they are not listed or readable until you switch back. If the provider already holds files, RPool refuses unless you confirm below."));
            ui.checkbox(
                &mut editor.existing_files_ok,
                tr("Change even if the provider already holds files"),
            );
            let encoding = FILENAME_ENCODINGS[editor.encoding.min(FILENAME_ENCODINGS.len() - 1)];
            let ready = !editor.crypt.is_empty() && !task.is_running();
            if ui
                .add_enabled(ready, egui::Button::new(tr("Change name encoding")))
                .clicked()
            {
                let args = args(&editor.crypt, encoding, editor.existing_files_ok);
                editor.notice = Some(match task.start_rpool(tr("Change name encoding"), rclone, args) {
                    Ok(()) => trf(
                        "Changing {crypt} to {encoding}. See Jobs for the result.",
                        &[("crypt", &editor.crypt), ("encoding", &encoding)],
                    ),
                    Err(error) => error,
                });
            }
            if let Some(notice) = &editor.notice {
                ui.label(notice);
            }
        });
    editor.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_encoding_has_a_label_note_and_parseable_command() {
        use clap::Parser;
        for encoding in FILENAME_ENCODINGS {
            assert!(!label(encoding).is_empty() && !note(encoding).is_empty());
            for ok in [false, true] {
                let argv = std::iter::once(OsString::from("rpool")).chain(args(
                    "gms_1_crypt:",
                    encoding,
                    ok,
                ));
                assert!(crate::cli::Cli::try_parse_from(argv).is_ok(), "{encoding}");
            }
        }
        assert_eq!(FILENAME_ENCODINGS[index_of("base32768")], "base32768");
        assert_eq!(index_of("unknown"), 0);
        let editor = Editor::open_for(&["a_crypt:".into()]);
        assert!(editor.open && editor.crypt == "a_crypt:");
    }
}
