//! A banner under the top bar when the configured rclone is missing or older
//! than RPool supports. Checked off the UI thread at start and whenever the
//! rclone executable setting changes.
use crate::doctor::rclone_version::{self, display, Support, MINIMUM, RECOMMENDED};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::storage::admin::{RcloneAdmin, ToolDiagnostics};
use eframe::egui;
use std::sync::mpsc::{Receiver, TryRecvError};

/// Result of `rclone version`: its first output line or the error text.
type Probe = Result<String, String>;

/// Background rclone version check and its dismissable banner; held by
/// `gui::app`.
#[derive(Default)]
pub(crate) struct RcloneVersionBanner {
    /// The rclone the last finished (or running) check was for.
    checked: Option<String>,
    /// Running check with the rclone it was started for.
    pending: Option<(String, Receiver<Probe>)>,
    /// Result of the last finished check.
    result: Option<Probe>,
    /// The user hid the banner; reset by the next finished check.
    dismissed: bool,
}

/// How serious a banner message is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    /// rclone works but is older than recommended.
    Warning,
    /// rclone is missing, did not run or is unsupported.
    Error,
}

impl RcloneVersionBanner {
    /// Collects a finished check and starts one for a changed `rclone`
    /// (one at a time, so typing a path does not start a process per key).
    pub(crate) fn poll(&mut self, rclone: &str) {
        if let Some((for_rclone, rx)) = &self.pending {
            match rx.try_recv() {
                Ok(probe) => {
                    self.checked = Some(for_rclone.clone());
                    self.result = Some(probe);
                    self.pending = None;
                    self.dismissed = false;
                }
                Err(TryRecvError::Disconnected) => {
                    self.checked = Some(for_rclone.clone());
                    self.pending = None;
                }
                Err(TryRecvError::Empty) => return,
            }
        }
        if self.checked.as_deref() == Some(rclone) || rclone.trim().is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let executable = rclone.to_string();
        std::thread::spawn(move || {
            let probe = RcloneAdmin::inherited(&executable)
                .version()
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(probe);
        });
        self.pending = Some((rclone.to_string(), rx));
    }

    /// A version check is running.
    pub(crate) fn is_checking(&self) -> bool {
        self.pending.is_some()
    }

    /// Draws the banner at the top when the last check reported a problem.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        if self.dismissed {
            return;
        }
        let Some((severity, message)) = self.result.as_ref().and_then(message) else {
            return;
        };
        let dark = ui.visuals().dark_mode;
        let (fill, text) = match severity {
            Severity::Warning => theme::warning_colors(dark),
            Severity::Error => theme::error_colors(dark),
        };
        egui::Panel::top("rclone-version-banner")
            .frame(
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(text, message);
                    if ui.small_button(tr("Dismiss")).clicked() {
                        self.dismissed = true;
                    }
                });
            });
    }
}

/// What the banner says for a check result; `None` when rclone is fine.
pub(crate) fn message(probe: &Probe) -> Option<(Severity, String)> {
    let line = match probe {
        Ok(line) => line,
        Err(error) => {
            return Some((
                Severity::Error,
                trf(
                    "rclone did not run ({error}). Set the rclone executable in Settings › General; RPool needs rclone {minimum} or newer.",
                    &[("error", error), ("minimum", &display(MINIMUM))],
                ),
            ))
        }
    };
    let version = rclone_version::parse(line);
    match rclone_version::support(version) {
        Support::Ok => None,
        Support::BelowRecommended => Some((
            Severity::Warning,
            trf(
                "rclone {version} works, but {recommended} or newer is recommended (security fix for rclone's remote control server).",
                &[
                    ("version", &version.map(display).unwrap_or_default()),
                    ("recommended", &display(RECOMMENDED)),
                ],
            ),
        )),
        Support::BelowMinimum => Some((
            Severity::Error,
            trf(
                "rclone {version} is too old: RPool needs {minimum} or newer. Update rclone before mounting or exporting diagnostics.",
                &[
                    ("version", &version.map(display).unwrap_or_default()),
                    ("minimum", &display(MINIMUM)),
                ],
            ),
        )),
        Support::Unknown => Some((
            Severity::Warning,
            trf(
                "Cannot read the rclone version. RPool needs rclone {minimum} or newer.",
                &[("minimum", &display(MINIMUM))],
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_only_for_old_unknown_or_missing_rclone() {
        assert_eq!(message(&Ok("rclone v1.75.1".into())), None);
        assert_eq!(
            message(&Ok("rclone v1.70.0".into())).map(|m| m.0),
            Some(Severity::Warning)
        );
        let old = message(&Ok("rclone v1.60.1-DEV".into())).unwrap();
        assert_eq!(old.0, Severity::Error);
        assert!(old.1.contains("v1.60.1") && old.1.contains("v1.64.0"));
        assert_eq!(
            message(&Err("not found".into())).map(|m| m.0),
            Some(Severity::Error)
        );
        assert_eq!(
            message(&Ok("something else".into())).map(|m| m.0),
            Some(Severity::Warning)
        );
    }

    #[test]
    fn a_missing_executable_is_reported_once_per_setting() {
        let mut banner = RcloneVersionBanner::default();
        let missing = "/nonexistent/rpool-test-rclone";
        banner.poll(missing);
        let started = std::time::Instant::now();
        while banner.is_checking() && started.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            banner.poll(missing);
        }
        assert!(!banner.is_checking());
        assert!(matches!(banner.result, Some(Err(_))));
        banner.poll(missing);
        assert!(!banner.is_checking(), "same setting is not checked again");
    }
}
