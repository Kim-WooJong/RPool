//! Add storage roots on one connection without relocating existing crypts.
//! The legacy default-location migration remains a separate, collapsed action.
use crate::config_sync::provision::EncryptionDefaults;
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::gui::usage_refresh::ProviderDetails;
use crate::storage::admin::domains::{DomainIdentity, DomainStore};
use eframe::egui;
use std::collections::BTreeMap;
use std::ffi::OsString;

pub(crate) const LOCATION_TASK: &str = "Change provider location";

#[derive(Debug, Default)]
pub(crate) struct Editor {
    pub(crate) open: bool,
    provider: String,
    current: String,
    path: String,
    move_existing: bool,
    new_name: String,
    new_path: String,
    backup_acknowledged: bool,
    domains: Option<DomainStore>,
    domains_loaded: bool,
    groups: BTreeMap<String, String>,
    notice: Option<String>,
}
impl Editor {
    pub(crate) fn open_for(provider: &str, root: Option<&str>) -> Self {
        Self {
            open: true,
            provider: provider.into(),
            current: format!("{provider}:{}", root.unwrap_or_default()),
            path: root.unwrap_or_default().into(),
            new_name: format!("{provider}_disk2_crypt"),
            ..Self::default()
        }
    }
}

/// Existing default migration, not the add-location operation.
pub(crate) fn args(provider: &str, path: &str, move_existing: bool) -> Vec<OsString> {
    let mut a = vec![
        "provider".into(),
        "location".into(),
        format!("--remote={}", provider.trim_end_matches(':')).into(),
        format!("--path={}", path.trim()).into(),
    ];
    if move_existing {
        a.push("--move-existing".into());
    }
    a
}
fn add_args(
    provider: &str,
    name: &str,
    path: &str,
    defaults: &EncryptionDefaults,
) -> Vec<OsString> {
    vec![
        "provider".into(),
        "encrypt".into(),
        format!("--provider={provider}").into(),
        format!("--name={}", name.trim()).into(),
        format!("--path={}", path.trim()).into(),
        format!("--entropy-bits={}", defaults.entropy_bits).into(),
        format!("--filename-encryption={}", defaults.filename_encryption).into(),
        format!("--filename-encoding={}", defaults.filename_encoding).into(),
        format!("--directory-encryption={}", defaults.directory_encryption).into(),
    ]
}

/// Merge only this dialog's declarations into the latest snapshot; reject stale rows.
fn merged_groups(
    mut live: DomainStore,
    before: &DomainStore,
    groups: &BTreeMap<String, String>,
) -> anyhow::Result<DomainStore> {
    for (name, group) in groups {
        let same = |a: Option<&DomainIdentity>, b: Option<&DomainIdentity>| {
            a.map(|x| (&x.capacity, &x.failure)) == b.map(|x| (&x.capacity, &x.failure))
        };
        if !same(live.remotes.get(name), before.remotes.get(name)) {
            anyhow::bail!(
                "Capacity identities changed elsewhere; reopen Storage locations before saving"
            );
        }
        if group.trim().is_empty() {
            live.remotes.remove(name);
        } else {
            live.remotes.insert(
                name.clone(),
                DomainIdentity {
                    capacity: group.trim().into(),
                    failure: String::new(),
                },
            );
        }
    }
    live.validate()?;
    Ok(live)
}

pub(crate) fn show(
    ctx: &egui::Context,
    editor: &mut Editor,
    rclone: &str,
    task: &mut TaskRunner,
    details: &ProviderDetails,
    defaults: &EncryptionDefaults,
    editable: bool,
) {
    if !editor.open {
        return;
    }
    if !editor.domains_loaded {
        editor.domains_loaded = true;
        match DomainStore::load() {
            Ok(store) => editor.domains = Some(store),
            Err(e) => editor.notice = Some(format!("{e:#}")),
        }
    }
    let crypts = details
        .crypts
        .get(&editor.provider)
        .cloned()
        .unwrap_or_default();
    for crypt in &crypts {
        let name = crypt.trim_end_matches(':');
        editor.groups.entry(name.into()).or_insert_with(|| {
            editor
                .domains
                .as_ref()
                .and_then(|s| s.remotes.get(name))
                .map(|d| d.capacity.clone())
                .unwrap_or_default()
        });
    }
    let supported = crate::storage::admin::catalog::path_capacity_supported(
        details
            .kinds
            .get(&editor.provider)
            .map(String::as_str)
            .unwrap_or_default(),
    );
    let ready = editable && !task.is_running();
    let mut open = true;
    egui::Window::new(tr("Storage locations")).id(egui::Id::new("provider-location"))
        .open(&mut open).resizable(true).default_width(560.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(580.0).show(ui, |ui| {
            ui.label(tr("One connection can use several disks. Adding a location keeps existing roots, keys and files unchanged."));
            ui.heading(tr("Existing storage locations"));
            for crypt in &crypts {
                ui.group(|ui| {
                    ui.monospace(crypt);
                    if let Some(backing) = details.locations.get(crypt) { ui.monospace(backing); }
                    let name = crypt.trim_end_matches(':');
                    ui.label(tr("Disk capacity group (optional)"));
                    if let Some(value) = editor.groups.get_mut(name) {
                        ui.add_enabled(ready && supported, egui::TextEdit::singleline(value).hint_text(tr("Empty: share the provider capacity")));
                    }
                });
            }
            ui.small(tr("Use the same group for paths on the same disk; different groups only for truly independent disks. Outage protection still uses the original connection."));
            if !supported { ui.small(tr("This backend keeps provider-wide capacity; per-location capacity groups are unavailable.")); }
            ui.small(tr("Capacity declarations are local to this PC and are not exported. Re-declare them after importing on another PC."));
            if ui.add_enabled(ready && supported && editor.domains.is_some(), egui::Button::new(tr("Save disk capacity groups"))).clicked() {
                let result = (|| -> anyhow::Result<DomainStore> {
                    let groups = crypts.iter().filter_map(|c| {
                        let name = c.trim_end_matches(':');
                        editor.groups.get(name).map(|g| (name.to_owned(), g.clone()))
                    }).collect();
                    let next = merged_groups(DomainStore::load()?, editor.domains.as_ref().unwrap(), &groups)?;
                    next.save()?;
                    Ok(next)
                })();
                match result {
                    Ok(next) => { editor.domains = Some(next); editor.notice = Some(tr("Capacity declarations saved. Recalculate pool capacity to validate the selected roots.").into()); },
                    Err(e) => editor.notice = Some(format!("{e:#}")),
                }
            }
            ui.separator();
            ui.heading(tr("Add storage location"));
            ui.label(tr("Location name (new encrypted remote)"));
            ui.add(egui::TextEdit::singleline(&mut editor.new_name));
            ui.label(tr("Physical folder on this connection"));
            ui.add(egui::TextEdit::singleline(&mut editor.new_path).hint_text("/disk2/rpool"));
            ui.small(tr("Leave empty to use the provider's root folder."));
            ui.small(tr("Select the new location in the pool or upload destination list after creation. No existing pool is changed automatically."));
            ui.checkbox(&mut editor.backup_acknowledged, tr("I understand that losing these keys loses access to my data"));
            if ui.add_enabled(ready && editor.backup_acknowledged && !editor.new_name.trim().is_empty(), egui::Button::new(tr("Add storage location"))).clicked() {
                let a = add_args(&editor.provider, &editor.new_name, &editor.new_path, defaults);
                editor.notice = Some(match task.start_rpool("Create encrypted provider", rclone, a) {
                    Ok(()) => tr("Creating encrypted provider. See Jobs for the result; refresh providers after completion.").into(),
                    Err(e) => e,
                });
            }
            ui.separator();
            ui.collapsing(tr("Change the existing default location"), |ui| {
                ui.label(tr("The folder on this provider where RPool stores its encrypted data. Encrypted providers that use the current folder move with it; keys stay the same."));
                ui.monospace(&editor.current);
                ui.label(trf("New folder on {provider}:", &[("provider", &editor.provider)]));
                ui.add(egui::TextEdit::singleline(&mut editor.path));
                ui.label(tr("If the current folder already holds files, RPool refuses unless you move them along. Moving needs an empty new folder and may take long for large data; stop mounts and uploads that use this provider first."));
                ui.checkbox(&mut editor.move_existing, tr("Move files already stored to the new folder"));
                if ui.add_enabled(ready, egui::Button::new(tr("Change location"))).clicked() {
                    editor.notice = Some(match task.start_rpool(LOCATION_TASK, rclone, args(&editor.provider, &editor.path, editor.move_existing)) {
                        Ok(()) => trf("Changing the location of {provider}. See Jobs for the result.", &[("provider", &editor.provider)]),
                        Err(e) => e,
                    });
                }
            });
            if let Some(notice) = &editor.notice { ui.label(notice); }
        });
    });
    editor.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn dialog_commands_parse() {
        for (path, moving) in [("/disk2/rpool", false), ("", true), (" -odd ", false)] {
            let argv = std::iter::once(OsString::from("rpool")).chain(args("nas:", path, moving));
            assert!(crate::cli::Cli::try_parse_from(argv).is_ok());
        }
        let e = Editor::open_for("nas", Some("/disk1"));
        assert!(e.open && e.current == "nas:/disk1" && e.path == "/disk1");
    }
    #[test]
    fn new_location_uses_explicit_path_and_existing_connection() {
        for path in ["/disk2/folder with spaces", "-odd", "", "/별도 디스크"] {
            let args = add_args("nas", "disk2_crypt", path, &EncryptionDefaults::default());
            let cli = crate::cli::Cli::try_parse_from(
                std::iter::once(OsString::from("rpool")).chain(args),
            )
            .unwrap();
            let Some(crate::cli::Commands::Provider(p)) = cli.command else {
                panic!("provider")
            };
            let crate::cli::ProviderCommands::Encrypt {
                provider,
                name,
                path: root,
                ..
            } = p.command
            else {
                panic!("encrypt")
            };
            assert_eq!(provider, "nas");
            assert_eq!(name, "disk2_crypt");
            assert_eq!(root.as_deref(), Some(path));
        }
    }
    #[test]
    fn capacity_edits_preserve_other_connections_and_reject_stale_rows() {
        let mut before = DomainStore::default();
        before.remotes.insert(
            "nas".into(),
            DomainIdentity {
                capacity: "disk1".into(),
                failure: "host".into(),
            },
        );
        let mut live = before.clone();
        live.remotes.insert(
            "other".into(),
            DomainIdentity {
                capacity: "other".into(),
                failure: "other-host".into(),
            },
        );
        let groups = BTreeMap::from([("disk2_crypt".into(), "disk2".into())]);
        let next = merged_groups(live, &before, &groups).unwrap();
        assert_eq!(next.remotes["nas"].failure, "host");
        assert!(next.remotes.contains_key("other"));
        assert_eq!(next.remotes["disk2_crypt"].failure, "");
        assert!(merged_groups(next, &before, &groups).is_err());
    }
}
