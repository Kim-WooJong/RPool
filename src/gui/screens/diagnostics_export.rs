//! Settings › General: "Export diagnostics…" runs `rpool doctor --bundle`.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;
use std::ffi::OsString;
use std::path::Path;

pub(crate) const TASK: &str = "Export diagnostics";

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let notice_id = egui::Id::new("diagnostics-export-notice");
    let mut notice: Option<String> = ui.ctx().data_mut(|data| data.get_temp(notice_id));
    theme::card_section(
        ui,
        tr("Diagnostics bundle"),
        Some(tr("A ZIP for bug reports: versions, doctor report, rclone and RPool settings without secrets, recent mount logs and monitoring status. Review it before sharing.")),
        |_| {},
        |ui| {
            ui.horizontal_wrapped(|ui| {
                let idle = !task.is_running();
                if theme::primary_button(ui, idle, tr("Export diagnostics…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name(default_file_name(crate::utils::now_unix()))
                        .add_filter(tr("ZIP archive"), &["zip"])
                        .save_file()
                    {
                        notice = Some(
                            match task.start_rpool(TASK, &state.settings.rclone, args(&path)) {
                                Ok(()) => trf(
                                    "Exporting to {path}. The task console shows the result.",
                                    &[("path", &path.display())],
                                ),
                                Err(error) => error,
                            },
                        );
                    }
                }
                if let Some(notice) = &notice {
                    ui.label(notice);
                }
            });
        },
    );
    ui.ctx().data_mut(|data| match notice {
        Some(notice) => {
            data.insert_temp(notice_id, notice);
        }
        None => data.remove::<String>(notice_id),
    });
}

/// `rpool doctor --bundle FILE`.
pub(crate) fn args(path: &Path) -> Vec<OsString> {
    vec!["doctor".into(), "--bundle".into(), path.into()]
}

/// `rpool-diagnostics-YYYYMMDD-HHMMSS.zip` (UTC).
pub(crate) fn default_file_name(unix: u64) -> String {
    let (year, month, day) = crate::monitor::history::civil_from_days((unix / 86_400) as i64);
    let seconds = unix % 86_400;
    format!(
        "rpool-diagnostics-{year:04}{month:02}{day:02}-{:02}{:02}{:02}.zip",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn runs_the_cli_bundle_command() {
        let args = args(Path::new("/tmp/d.zip"));
        assert_eq!(
            args,
            ["doctor", "--bundle", "/tmp/d.zip"].map(OsString::from)
        );
        let cli =
            crate::cli::Cli::try_parse_from(std::iter::once(OsString::from("rpool")).chain(args))
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::cli::Commands::Doctor(crate::cli::DoctorArgs {
                bundle: Some(_),
                ..
            }))
        ));
    }

    #[test]
    fn default_name_is_a_utc_timestamp() {
        assert_eq!(
            default_file_name(1_727_740_800 + 3_723),
            "rpool-diagnostics-20241001-010203.zip"
        );
    }
}
