//! Unsaved WebDAV writes a mount recovered from rclone's cache: files put
//! back, recovered copies to review, and entries kept in the cache folder.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::mount::cache_recovery::{RecoveryReport, REPORT};
use eframe::egui;
use std::path::{Path, PathBuf};

/// `<workspace>/.rpool/<REPORT>`; `None` unless the workspace path is absolute.
fn report_path(workspace: &str) -> Option<PathBuf> {
    let workspace = Path::new(workspace.trim());
    workspace
        .is_absolute()
        .then(|| workspace.join(".rpool").join(REPORT))
}

/// Reads the cache-recovery reports of `workspace`; empty when missing or
/// unreadable. Called from `MountSession::poll`.
pub(super) fn load(workspace: &str) -> Vec<RecoveryReport> {
    report_path(workspace)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Hides the report. The recovered files and any kept cache folder remain.
fn dismiss(workspace: &str) -> std::io::Result<()> {
    match report_path(workspace) {
        Some(path) if path.exists() => std::fs::remove_file(path),
        _ => Ok(()),
    }
}

/// Collapsible "Recovered unsaved files" block on the Drive overview, with a
/// Dismiss button that removes the report file; hidden when there is none.
pub(super) fn show(ui: &mut egui::Ui, form: &mut MountForm) {
    if form.session.cache_recovery.is_empty() {
        return;
    }
    let reports = &form.session.cache_recovery;
    let copies: usize = reports.iter().map(|r| r.copied.len()).sum();
    let kept: usize = reports.iter().map(|r| r.kept.len()).sum();
    let restored: usize = reports.iter().map(|r| r.imported.len()).sum();
    let title = trf(
        "Recovered unsaved files · {restored} restored · {copies} copies to review · {kept} kept",
        &[
            ("restored", &restored),
            ("copies", &copies),
            ("kept", &kept),
        ],
    );
    let mut dismissed = false;
    egui::CollapsingHeader::new(title)
        .id_salt("mount-cache-recovery")
        .default_open(copies + kept > 0)
        .show(ui, |ui| {
            ui.small(tr("A previous WebDAV mount stopped before rclone saved these writes. Restored files are pending upload like any other save. Recovered copies sit next to a file that may have changed since; compare and keep what you need. Nothing was overwritten."));
            for report in reports {
                for path in &report.imported {
                    ui.label(trf("Restored: {path}", &[("path", path)]));
                }
                for (from, to) in &report.copied {
                    ui.horizontal(|ui| {
                        ui.label(trf("Copy of {from}: {to}", &[("from", from), ("to", to)]));
                        if ui.small_button(tr("Copy path")).clicked() {
                            ui.ctx().copy_text(to.clone());
                        }
                    });
                }
                if !report.kept.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(trf("Kept in {dir}", &[("dir", &report.dir.display())]));
                        if ui.small_button(tr("Copy folder path")).clicked() {
                            ui.ctx().copy_text(report.dir.display().to_string());
                        }
                    });
                    for (path, reason) in &report.kept {
                        let path = if path.is_empty() { tr("(whole cache)") } else { path };
                        ui.small(format!("  {path}: {reason}"));
                    }
                    ui.small(tr("Kept entries were not imported (incomplete or unreadable). Inspect the folder and delete it yourself when done."));
                }
            }
            if ui.button(tr("Dismiss report")).clicked() {
                dismissed = true;
            }
        });
    if dismissed {
        match dismiss(&form.workspace) {
            Ok(()) => form.session.cache_recovery.clear(),
            Err(e) => {
                form.session.notice = Some(trf(
                    "Cannot dismiss recovery report: {error}",
                    &[("error", &e)],
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips_and_dismiss_removes_only_the_report() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().to_str().unwrap().to_string();
        assert!(load(&workspace).is_empty());
        assert!(load("relative/ws").is_empty());
        let report = RecoveryReport {
            dir: root.path().join("recovered-native-cache/x"),
            copied: vec![("a.txt".into(), "a (recovered x-1).txt".into())],
            ..Default::default()
        };
        std::fs::create_dir_all(root.path().join(".rpool")).unwrap();
        std::fs::write(
            root.path().join(".rpool").join(REPORT),
            serde_json::to_vec(&vec![report.clone()]).unwrap(),
        )
        .unwrap();
        assert_eq!(load(&workspace), [report]);
        dismiss(&workspace).unwrap();
        assert!(load(&workspace).is_empty());
        assert!(root.path().join(".rpool").exists());
    }
}
