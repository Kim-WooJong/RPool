use eframe::egui;
use std::collections::BTreeMap;

pub(crate) fn remote_selector(
    ui: &mut egui::Ui,
    selected: &mut Vec<String>,
    discovered: &[String],
    default_remote_path: &str,
    remote_roots: &BTreeMap<String, String>,
    manual_remote: &mut String,
) {
    ui.label("Encrypted cloud destinations");
    ui.small("rpool accepts rclone crypt remotes only. Per-remote default paths override the global GUI default folder.");

    let mut remove_index = None;
    for (index, remote) in selected.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(remote).desired_width(f32::INFINITY));
            if ui.button("Remove").clicked() {
                remove_index = Some(index);
            }
        });
    }
    if let Some(index) = remove_index {
        selected.remove(index);
    }

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(manual_remote)
                .hint_text("crypt-remote:path")
                .desired_width(f32::INFINITY),
        );
        if ui.button("Add").clicked() {
            let value = manual_remote.trim();
            if !value.is_empty() && !selected.iter().any(|existing| existing == value) {
                selected.push(value.to_string());
                manual_remote.clear();
            }
        }
    });

    if !discovered.is_empty() {
        ui.separator();
        ui.label("Discovered rclone crypt remotes");
        for remote in discovered {
            let path = configured_or_default_path(remote, default_remote_path, remote_roots);
            let target = append_default_path(remote, path);
            ui.horizontal(|ui| {
                ui.monospace(remote);
                ui.label("→");
                ui.monospace(&target);
                let already_selected = selected.iter().any(|existing| existing == &target);
                if ui
                    .add_enabled(!already_selected, egui::Button::new("Add target"))
                    .clicked()
                {
                    selected.push(target);
                }
            });
        }
    } else {
        ui.small("No rclone crypt remotes were discovered. Configure a crypt remote before storing data.");
    }
}

fn configured_or_default_path<'a>(
    remote: &str,
    default_remote_path: &'a str,
    remote_roots: &'a BTreeMap<String, String>,
) -> &'a str {
    let name = remote
        .split_once(':')
        .map(|(name, _)| name)
        .unwrap_or(remote);
    remote_roots
        .get(name)
        .map(String::as_str)
        .unwrap_or(default_remote_path)
}

fn append_default_path(remote: &str, path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return remote.to_string();
    }
    let (_, existing_path) = match remote.split_once(':') {
        Some(parts) => parts,
        None => return remote.to_string(),
    };
    if !existing_path.trim().is_empty() {
        return remote.to_string();
    }
    if remote.ends_with(':') {
        format!("{remote}{path}")
    } else {
        format!("{remote}/{path}")
    }
}
