//! Account capacity / outage identities as an editable table. One row per
//! backing remote; the same capacity group means a shared quota, the same
//! outage group means accounts that can fail together.
use super::form::MountForm;
use crate::gui::state::GuiState;
use crate::gui::widgets::{section_header, toolbar};
use crate::storage::admin::domains::{DomainIdentity, DomainStore};
use eframe::egui;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct IdentityRow {
    pub(super) remote: String,
    pub(super) capacity: String,
    pub(super) failure: String,
}

/// Saved declarations, or none if the store is missing or unreadable.
pub(super) fn load() -> Vec<IdentityRow> {
    DomainStore::load()
        .map(|store| rows(&store))
        .unwrap_or_default()
}

pub(super) fn rows(store: &DomainStore) -> Vec<IdentityRow> {
    store
        .remotes
        .iter()
        .map(|(remote, id)| IdentityRow {
            remote: remote.clone(),
            capacity: id.capacity.clone(),
            failure: id.failure.clone(),
        })
        .collect()
}

/// Rows with an empty remote are ignored; a row needs a capacity group.
pub(super) fn store(rows: &[IdentityRow]) -> anyhow::Result<DomainStore> {
    let mut store = DomainStore::default();
    for row in rows.iter().filter(|r| !r.remote.trim().is_empty()) {
        let remote = row.remote.trim().trim_end_matches(':').to_string();
        if row.capacity.trim().is_empty() {
            anyhow::bail!("{remote}: enter a capacity group (use the same one for accounts that share a quota)");
        }
        let identity = DomainIdentity {
            capacity: row.capacity.trim().into(),
            failure: row.failure.trim().into(),
        };
        if store.remotes.insert(remote.clone(), identity).is_some() {
            anyhow::bail!("{remote} is listed twice");
        }
    }
    store.validate()?;
    Ok(store)
}

/// Adds an empty row for every measured backing remote that has none yet.
pub(super) fn add_measured(form: &mut MountForm) {
    let Some(capacity) = &form.capacity else {
        return;
    };
    for target in &capacity.targets {
        if !form.identities.iter().any(|r| r.remote == target.backing) {
            form.identities.push(IdentityRow {
                remote: target.backing.clone(),
                ..Default::default()
            });
        }
    }
}

fn undeclared(form: &MountForm) -> usize {
    form.identities
        .iter()
        .filter(|r| {
            !r.remote.is_empty() && (r.capacity.trim().is_empty() || r.failure.trim().is_empty())
        })
        .count()
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    add_measured(form);
    let open = undeclared(form) > 0 || form.identities.is_empty();
    egui::CollapsingHeader::new("Account identities")
        .id_salt("mount-identities")
        .default_open(open)
        .show(ui, |ui| {
            section_header(
                ui,
                "Account identities",
                Some("Tell RPool which remotes share a quota and which can fail together. Needed before free space is combined or resilient placement can write."),
            );
            let mut remove = None;
            ui.add_enabled_ui(!form.runner.is_running(), |ui| {
                egui::Grid::new("mount-identity-grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.strong("Backing remote");
                        ui.strong("Capacity group").on_hover_text("Same value = one shared quota (aliases, folders of one account). Different values = independent accounts.");
                        ui.strong("Outage group").on_hover_text("Accounts that can go down together, for example the same provider. Resilient placement limits shards per outage group.");
                        ui.label("");
                        ui.end_row();
                        for (index, row) in form.identities.iter_mut().enumerate() {
                            ui.add(egui::TextEdit::singleline(&mut row.remote).desired_width(180.0).hint_text("e.g. gdrive1"));
                            ui.add(egui::TextEdit::singleline(&mut row.capacity).desired_width(160.0).hint_text("e.g. gdrive1-account"));
                            ui.add(egui::TextEdit::singleline(&mut row.failure).desired_width(140.0).hint_text("e.g. google"));
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                            ui.end_row();
                        }
                    });
                toolbar(ui, |ui| {
                    if ui.button("Add row").clicked() {
                        form.identities.push(IdentityRow::default());
                    }
                    if ui.button("Save identities").clicked() {
                        form.notice = Some(match store(&form.identities).and_then(|s| s.save()) {
                            Ok(()) => {
                                form.capacity = None;
                                state.pools.invalidate_capacity();
                                "Identities saved. Check capacity to measure with them.".into()
                            }
                            Err(error) => format!("{error:#}"),
                        });
                    }
                    let missing = undeclared(form);
                    if missing > 0 {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("{missing} row(s) incomplete"));
                    }
                });
            });
            if let Some(index) = remove {
                form.identities.remove(index);
            }
            ui.small("Use the backing (physical) remote name, not the crypt remote. No passwords or tokens are stored here.");
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(remote: &str, capacity: &str, failure: &str) -> IdentityRow {
        IdentityRow {
            remote: remote.into(),
            capacity: capacity.into(),
            failure: failure.into(),
        }
    }

    #[test]
    fn rows_round_trip_through_the_store() {
        let input = vec![
            row("gdrive1", "g1", "google"),
            row("box1:", "b1", ""),
            row("  ", "", ""),
        ];
        let saved = store(&input).unwrap();
        assert_eq!(saved.remotes.len(), 2);
        assert_eq!(saved.remotes["box1"].capacity, "b1");
        let back = rows(&saved);
        assert_eq!(back[0], row("box1", "b1", ""));
        assert_eq!(back[1], row("gdrive1", "g1", "google"));
    }

    #[test]
    fn incomplete_or_duplicate_rows_are_refused() {
        assert!(store(&[row("a", "", "x")]).is_err());
        assert!(store(&[row("a", "1", ""), row("a:", "2", "")]).is_err());
        assert!(
            store(&[row("a:path", "1", "")]).is_err(),
            "backing remote names only"
        );
    }
}
