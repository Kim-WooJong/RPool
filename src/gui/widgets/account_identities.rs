//! Account capacity / outage identities for a pool's backing remotes, edited
//! where the pool is defined. Identities are global per backing remote: the
//! same capacity group means one shared quota, the same outage group means
//! accounts that can fail together.
use crate::pool::capacity::BackingRemote;
use crate::storage::admin::domains::{DomainIdentity, DomainStore};
use eframe::egui;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IdentityRow {
    pub(crate) remote: String,
    /// rclone backend type, shown for orientation and used by Suggest.
    pub(crate) kind: String,
    pub(crate) capacity: String,
    pub(crate) failure: String,
}
impl IdentityRow {
    fn complete(&self) -> bool {
        !self.capacity.trim().is_empty() && !self.failure.trim().is_empty()
    }
}

/// One row per distinct backing remote of the pool, filled from `store`.
pub(crate) fn pool_rows(store: &DomainStore, backings: &[BackingRemote]) -> Vec<IdentityRow> {
    let mut rows: Vec<IdentityRow> = Vec::new();
    for backing in backings {
        if rows.iter().any(|r| r.remote == backing.backing) {
            continue;
        }
        let saved = store.remotes.get(&backing.backing);
        rows.push(IdentityRow {
            remote: backing.backing.clone(),
            kind: backing.kind.clone(),
            capacity: saved.map(|i| i.capacity.clone()).unwrap_or_default(),
            failure: saved.map(|i| i.failure.clone()).unwrap_or_default(),
        });
    }
    rows
}

/// Proposes identities for empty fields only: each backing remote its own
/// account, and one outage group per provider type. Nothing is saved.
pub(crate) fn suggest(rows: &mut [IdentityRow]) {
    for row in rows {
        if row.capacity.trim().is_empty() {
            row.capacity = row.remote.clone();
        }
        if row.failure.trim().is_empty() {
            row.failure = if row.kind.is_empty() {
                row.remote.clone()
            } else {
                row.kind.clone()
            };
        }
    }
}

/// `store` with the pool's rows applied. Other remotes are kept; a row with
/// both fields empty removes that remote's declaration.
pub(crate) fn merged(mut store: DomainStore, rows: &[IdentityRow]) -> anyhow::Result<DomainStore> {
    for row in rows {
        let (capacity, failure) = (row.capacity.trim(), row.failure.trim());
        if capacity.is_empty() && failure.is_empty() {
            store.remotes.remove(&row.remote);
            continue;
        }
        if capacity.is_empty() {
            anyhow::bail!(
                "{}: enter a capacity group (the same one for accounts that share a quota)",
                row.remote
            );
        }
        store.remotes.insert(
            row.remote.clone(),
            DomainIdentity {
                capacity: capacity.into(),
                failure: failure.into(),
            },
        );
    }
    store.validate()?;
    Ok(store)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditorAction {
    None,
    Save,
}

/// The table for a pool's backing remotes.
pub(crate) fn editor(
    ui: &mut egui::Ui,
    id: &str,
    rows: &mut [IdentityRow],
    enabled: bool,
) -> EditorAction {
    let mut action = EditorAction::None;
    if rows.is_empty() {
        ui.small("Calculate capacity to list this pool's backing accounts.");
        return action;
    }
    ui.add_enabled_ui(enabled, |ui| {
        egui::Grid::new(id).num_columns(4).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.strong("Backing account");
            ui.strong("Type");
            ui.strong("Capacity group").on_hover_text("Same value = one shared quota (aliases or folders of one account). Different values = independent accounts whose free space adds up.");
            ui.strong("Outage group").on_hover_text("Accounts that can go down together, usually one per provider. Resilient placement keeps at most parity-many shards per group.");
            ui.end_row();
            for row in rows.iter_mut() {
                ui.label(&row.remote);
                ui.label(egui::RichText::new(&row.kind).weak());
                ui.add(egui::TextEdit::singleline(&mut row.capacity).desired_width(150.0).hint_text("e.g. dropbox-main"));
                ui.add(egui::TextEdit::singleline(&mut row.failure).desired_width(130.0).hint_text("e.g. dropbox"));
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Suggest").on_hover_text("Fills empty fields: each account independent, one outage group per provider type. Review before saving.").clicked() {
                suggest(rows);
            }
            if ui.button("Save identities").clicked() {
                action = EditorAction::Save;
            }
            let missing = rows.iter().filter(|r| !r.complete()).count();
            if missing > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{missing} account(s) not fully declared"));
            }
        });
    });
    ui.small("Identities apply to every pool that uses these accounts. Use different capacity groups only for truly separate accounts.");
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backing(remote: &str, backing: &str, kind: &str) -> BackingRemote {
        BackingRemote {
            remote: remote.into(),
            backing: backing.into(),
            kind: kind.into(),
        }
    }

    fn store(entries: &[(&str, &str, &str)]) -> DomainStore {
        let mut store = DomainStore::default();
        for (remote, capacity, failure) in entries {
            store.remotes.insert(
                (*remote).into(),
                DomainIdentity {
                    capacity: (*capacity).into(),
                    failure: (*failure).into(),
                },
            );
        }
        store
    }

    #[test]
    fn pool_rows_list_each_backing_once_with_saved_values() {
        let saved = store(&[("dropbox", "dbx", "dropbox"), ("other", "o", "o")]);
        let rows = pool_rows(
            &saved,
            &[
                backing("dropbox_crypt:", "dropbox", "dropbox"),
                backing("dropbox_crypt:photos", "dropbox", "dropbox"),
                backing("koofr_crypt:", "koofr", "koofr"),
            ],
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].capacity.as_str(), rows[0].failure.as_str()),
            ("dbx", "dropbox")
        );
        assert!(rows[1].capacity.is_empty() && rows[1].kind == "koofr");
    }

    #[test]
    fn suggest_fills_only_empty_fields() {
        let mut rows = pool_rows(
            &store(&[("mikrotik", "mt", "")]),
            &[
                backing("a:", "mikrotik", "s3"),
                backing("b:", "filen", "filen"),
            ],
        );
        suggest(&mut rows);
        assert_eq!(
            (rows[0].capacity.as_str(), rows[0].failure.as_str()),
            ("mt", "s3")
        );
        assert_eq!(
            (rows[1].capacity.as_str(), rows[1].failure.as_str()),
            ("filen", "filen")
        );
    }

    #[test]
    fn saving_keeps_other_remotes_and_clearing_removes_a_declaration() {
        let existing = store(&[("elsewhere", "e", "e"), ("filen", "f", "f")]);
        let rows = vec![
            IdentityRow {
                remote: "filen".into(),
                ..Default::default()
            },
            IdentityRow {
                remote: "koofr".into(),
                kind: "koofr".into(),
                capacity: "k".into(),
                failure: "koofr".into(),
            },
        ];
        let saved = merged(existing, &rows).unwrap();
        assert!(saved.remotes.contains_key("elsewhere"));
        assert!(
            !saved.remotes.contains_key("filen"),
            "cleared row is undeclared"
        );
        assert_eq!(saved.remotes["koofr"].capacity, "k");
        let bad = vec![IdentityRow {
            remote: "x".into(),
            failure: "g".into(),
            ..Default::default()
        }];
        assert!(merged(DomainStore::default(), &bad).is_err());
    }
}
