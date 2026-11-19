//! Regression cases for the shared GUI/CLI merge; no backend or store I/O.
use super::{catalog::RemoteCatalog, domains::{DomainIdentity, DomainStore}, location_capacity::merged};
use crate::prelude::*;

fn catalog(extra: Value) -> RemoteCatalog {
    let mut config = serde_json::json!({
        "disk": {"type": "sftp"},
        "selected": {"type": "crypt", "remote": "disk:/one"}
    });
    config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    RemoteCatalog::parse(&config).unwrap()
}
fn edit(names: &[&str]) -> BTreeMap<String, Option<String>> {
    names.iter().map(|name| ((*name).into(), Some("new-disk".into()))).collect()
}
#[test]
fn unrelated_broken_or_ineligible_locations_do_not_block_save() {
    let c = catalog(serde_json::json!({
        "dangling": {"type": "crypt", "remote": "missing:"},
        "off": {"type": "crypt", "remote": "missing:", "no_data_encryption": true},
        "native": {"type": "crypt", "remote": "/native/path"},
        "cloud": {"type": "box"},
        "cloud_crypt": {"type": "crypt", "remote": "cloud:folder"},
        "cycle": {"type": "alias", "remote": "cycle:"},
        "cycle_crypt": {"type": "crypt", "remote": "cycle:"}
    }));
    let mut live = DomainStore::default();
    live.remotes.insert("selected".into(), DomainIdentity { capacity: "old-disk".into(), failure: "legacy-failure".into() });
    live.remotes.insert("cloud_crypt".into(), DomainIdentity { capacity: "invalid-cloud-override".into(), failure: String::new() });
    let result = merged(live.clone(), &live, &c, &c, &edit(&["selected"])).unwrap();
    assert_eq!(result.remotes["selected"].capacity, "new-disk");
    assert_eq!(result.remotes["selected"].failure, "legacy-failure");
    assert_eq!(result.remotes["cloud_crypt"].capacity, "invalid-cloud-override");
    assert!(merged(live.clone(), &live, &c, &c, &edit(&["dangling"])).is_err());
}
#[test]
fn matching_wrapper_and_encryption_off_peers_still_conflict() {
    for peer in [
        serde_json::json!({"type": "crypt", "remote": "disk:/one/./", "no_data_encryption": true}),
        serde_json::json!({"type": "crypt", "remote": "alias:one"}),
        serde_json::json!({"type": "crypt", "remote": "selected:ignored-encrypted-suffix"})
    ] {
        let c = catalog(serde_json::json!({"alias": {"type": "alias", "remote": "disk:/"}, "peer": peer}));
        let mut live = DomainStore::default();
        live.remotes.insert("peer".into(), DomainIdentity { capacity: "other-disk".into(), failure: String::new() });
        assert!(merged(live.clone(), &live, &c, &c, &edit(&["selected"])).is_err());
    }
}
#[test]
fn simultaneous_same_root_edits_and_stale_rows_keep_existing_policy() {
    let c = catalog(serde_json::json!({"peer": {"type": "crypt", "remote": "disk:/one/./"}}));
    let before = DomainStore::default();
    let result = merged(before.clone(), &before, &c, &c, &edit(&["selected", "peer"])).unwrap();
    assert_eq!(result.remotes["selected"].capacity, result.remotes["peer"].capacity);
    assert!(merged(result, &before, &c, &c, &edit(&["selected"])).is_err());
    let changed = catalog(serde_json::json!({"selected": {"type": "crypt", "remote": "disk:/two"}}));
    assert!(merged(before.clone(), &before, &changed, &c, &edit(&["selected"])).is_err());
}
