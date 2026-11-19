//! Standalone PC-local capacity query/edit, without mounts or storage I/O.
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::admin::{domains::DomainStore, location_capacity};

/// Query or explicitly edit one local declaration; never starts a data operation.
pub(crate) fn run(rclone: &str, remote: &str, group: Option<String>, inherit: bool, json: bool) -> Result<()> {
    anyhow::ensure!(!(group.is_some() && inherit), "choose --group or --inherit, not both");
    let admin = RcloneAdmin::inherited(rclone);
    let original = admin.catalog()?;
    let before = DomainStore::load()?;
    let changing = group.is_some() || inherit;
    let store = if changing {
        let edits = BTreeMap::from([(remote.to_owned(), group)]);
        let catalog = admin.catalog()?;
        let live = DomainStore::load()?;
        let next = location_capacity::merged(live, &before, &catalog, &original, &edits)?;
        // Atomic replacement only; no rclone config, account or pool writes.
        next.save()?;
        next
    } else { before };
    let view = location_capacity::describe(&original, &store, remote)?;
    if json { println!("{}", serde_json::to_string_pretty(&view)?); }
    else {
        println!("{}: {} ({}), backing {}", view.remote, view.effective_group, view.mode, view.backing);
        println!("Same group locations: {}", view.same_group_locations.join(", "));
        println!("PC-local; not exported. Re-declare on other PCs. Only new jobs/new plans use changes; existing immutable plans are unchanged. No stop/restart/relocation is required.");
    }
    Ok(())
}
