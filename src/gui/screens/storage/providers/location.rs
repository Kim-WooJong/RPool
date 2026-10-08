//! The "Location…" dialog of a provider card: where RPool stores data on the
//! provider (its default path). Starts `provider location`, which also
//! repoints the crypt remotes that wrap the old folder.
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

/// Jobs name of `provider location`; `gui::app` reloads the default paths
/// and the provider list when it finishes.
pub(crate) const LOCATION_TASK: &str = "Change provider location";

/// State of the "Location" dialog.
#[derive(Debug, Default)]
pub(crate) struct Editor {
    /// Whether the dialog is shown.
    pub(crate) open: bool,
    /// Base provider the dialog was opened for.
    provider: String,
    /// Current location as shown when opened (`provider:path`).
    current: String,
    /// New folder on the provider; empty for its root.
    path: String,
    /// Pass `--move-existing`: move stored files to the new folder.
    move_existing: bool,
    /// Start result, shown in the dialog.
    notice: Option<String>,
}

impl Editor {
    /// Opens the dialog for `provider` with its current default path, if any.
    pub(crate) fn open_for(provider: &str, root: Option<&str>) -> Self {
        Self {
            open: true,
            provider: provider.to_string(),
            current: format!("{provider}:{}", root.unwrap_or_default()),
            path: root.unwrap_or_default().to_string(),
            ..Self::default()
        }
    }
}

/// `provider location` arguments for the dialog's choice.
pub(crate) fn args(provider: &str, path: &str, move_existing: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "provider".into(),
        "location".into(),
        format!("--remote={}", provider.trim_end_matches(':')).into(),
        format!("--path={}", path.trim()).into(),
    ];
    if move_existing {
        args.push("--move-existing".into());
    }
    args
}

/// Draws the "Location" dialog and starts `provider location`.
pub(crate) fn show(ctx: &egui::Context, editor: &mut Editor, rclone: &str, task: &mut TaskRunner) {
    if !editor.open {
        return;
    }
    let mut open = true;
    egui::Window::new(tr("Storage location"))
        .id(egui::Id::new("provider-location"))
        .open(&mut open)
        .resizable(true)
        .default_width(500.0)
        .show(ctx, |ui| {
            ui.label(tr("The folder on this provider where RPool stores its encrypted data. Encrypted providers that use the current folder move with it; keys stay the same."));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Current"));
                ui.monospace(&editor.current);
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(trf("New folder on {provider}:", &[("provider", &editor.provider)]));
                ui.add(
                    egui::TextEdit::singleline(&mut editor.path)
                        .hint_text("/disk2/rpool")
                        .desired_width(ui.available_width().min(300.0)),
                );
            });
            ui.small(tr("Leave empty to use the provider's root folder."));
            ui.separator();
            ui.label(tr("If the current folder already holds files, RPool refuses unless you move them along. Moving needs an empty new folder and may take long for large data; stop mounts and uploads that use this provider first."));
            ui.checkbox(
                &mut editor.move_existing,
                tr("Move files already stored to the new folder"),
            );
            let ready = !task.is_running();
            if ui
                .add_enabled(ready, egui::Button::new(tr("Change location")))
                .clicked()
            {
                let args = args(&editor.provider, &editor.path, editor.move_existing);
                editor.notice = Some(match task.start_rpool(LOCATION_TASK, rclone, args) {
                    Ok(()) => trf(
                        "Changing the location of {provider}. See Jobs for the result.",
                        &[("provider", &editor.provider)],
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
    fn dialog_commands_parse() {
        use clap::Parser;
        for (path, moving) in [("/disk2/rpool", false), ("", true), (" -odd ", false)] {
            let argv = std::iter::once(OsString::from("rpool")).chain(args("nas:", path, moving));
            assert!(crate::cli::Cli::try_parse_from(argv).is_ok(), "{path}");
        }
        let editor = Editor::open_for("nas", Some("/disk1"));
        assert!(editor.open && editor.current == "nas:/disk1" && editor.path == "/disk1");
        assert_eq!(Editor::open_for("nas", None).current, "nas:");
    }
}
