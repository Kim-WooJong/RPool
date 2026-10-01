//! What drive history (`crate::drive_history`) uses from the mount: opening
//! a pool-sync workspace, its v6 events, and writing a
//! restored revision. Every write is an ordinary new revision of the drive
//! (the same records a mounted PC publishes); nothing here removes history.
pub(crate) use super::namespace::valid_path;
pub(crate) use super::shared_model::{Content, Event};
pub(crate) use super::shared_transport::SharedTransport as Transport;
#[cfg(test)]
pub(crate) use super::virtual_drive::Revision;
pub(crate) use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

/// Clean shard cache of a drive history session (reads during restore).
const CACHE_BYTES: u64 = 1 << 30;

#[derive(Deserialize)]
struct Binding {
    version: u32,
    pool: String,
}

/// Opens an existing, unmounted pool-sync workspace of `pool` (takes
/// its lock, like a mount; fails while it is mounted).
pub(crate) fn open_workspace(rclone: &str, pool: &str, root: &Path) -> Result<VirtualDrive> {
    let binding: Binding = crate::utils::read_json(&root.join("virtual.json"))
        .with_context(|| format!("not an RPool online-drive workspace: {}", root.display()))?;
    if binding.pool != pool {
        bail!("workspace belongs to pool {}, not {pool}", binding.pool);
    }
    if binding.version != super::virtual_drive::FORMAT_VERSION {
        bail!("trash/versions/rollback need an automatic pool-sync (v6) workspace");
    }
    let worker: String = crate::utils::read_json(&root.join("pool-worker.json"))
        .context("workspace has no pool worker label; mount it once first")?;
    VirtualDrive::open(rclone, pool, root, &worker, CACHE_BYTES)
}

/// A fresh scratch workspace on the pool's newest drive generation, used to
/// publish history operations without a workspace (as worker `worker`).
/// `None`: the pool has no pool-sync drive.
pub(crate) fn open_scratch(
    rclone: &str,
    pool: &str,
    root: &Path,
    worker: &str,
    generation: &crate::pool::browse_generations::Generation,
) -> Result<VirtualDrive> {
    let drive = match &generation.epoch {
        Some(epoch) => {
            VirtualDrive::open_with_epoch(rclone, pool, root, worker, CACHE_BYTES, epoch)?
        }
        None => VirtualDrive::open(rclone, pool, root, worker, CACHE_BYTES)?,
    };
    drive.pull()?;
    durable_worker(&drive.root, worker)?;
    Ok(drive)
}

fn durable_worker(root: &Path, worker: &str) -> Result<()> {
    super::namespace::durable_json(&root.join("pool-worker.json"), &worker)
}

pub(crate) fn roots(drive: &VirtualDrive) -> &[String] {
    &drive.pool_sync_roots
}
pub(crate) fn worker(drive: &VirtualDrive) -> String {
    drive.state.lock().unwrap().worker.clone()
}

/// v6 events this workspace knows, and those not yet published.
pub(crate) fn v6_events(drive: &VirtualDrive) -> (BTreeMap<String, Event>, BTreeSet<String>) {
    let state = drive.state.lock().unwrap();
    let unpublished = state
        .events
        .keys()
        .filter(|id| !state.published.contains(*id))
        .cloned()
        .collect();
    (state.events.clone(), unpublished)
}

/// Manifests this open drive may still read besides its events' current
/// view: open files (read pins) and the parents of local writes not
/// uploaded yet (an incremental upload reuses its base's shards).
/// The drive cleanup keeps them (`drive_history::cleanup`).
pub(crate) fn kept_contents(drive: &VirtualDrive) -> Vec<(String, Content)> {
    let mut out = Vec::new();
    for (path, revision) in drive.pins.lock().unwrap().iter() {
        if let super::virtual_drive::Revision::Cloud { id, content } = revision {
            out.push((format!("open file {path} ({id})"), content.clone()));
        }
    }
    let peer: Vec<String> = drive
        .peer_read_pins
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    let state = drive.state.lock().unwrap();
    let mut ids: BTreeSet<String> = peer.into_iter().collect();
    for intent in &state.pending {
        ids.extend(intent.parents.iter().cloned());
        if let Some(previous) = intent
            .depends_on
            .as_ref()
            .and_then(|id| state.committed_intents.get(id))
        {
            ids.insert(previous.clone());
        }
    }
    for id in ids {
        if let Some(content) = state.events.get(&id).and_then(|e| e.content.as_ref()) {
            out.push((format!("local drive state ({id})"), content.clone()));
        }
    }
    out
}

/// Paths with local writes not yet uploaded (history operations refuse them).
pub(crate) fn pending_paths(drive: &VirtualDrive) -> BTreeSet<String> {
    drive
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .map(|intent| intent.path.clone())
        .collect()
}

impl VirtualDrive {
    /// A history change is based on what the drive shows now: record the
    /// visible revision of `path` as its edit baseline (what reading the file
    /// does).
    pub(crate) fn history_base_on_visible(&self, path: &str) -> Result<()> {
        let Some(super::virtual_drive::Revision::Cloud { id, .. }) =
            self.view()?.get(path).cloned()
        else {
            return Ok(());
        };
        let mut s = self.state.lock().unwrap();
        let mut next = s.clone();
        next.bases.insert(path.into(), vec![id]);
        next.save(&self.root)?;
        *s = next;
        Ok(())
    }

    /// v6: makes an existing revision's content the new content of `path`
    /// without moving bytes, exactly like a move does: one new namespace
    /// event (descending from what is at `path` now) referencing the same
    /// immutable manifest. Older RPool reads it as a normal edit.
    pub(crate) fn history_put_content(&self, path: &str, content: Content) -> Result<String> {
        if self.pool_sync_roots.is_empty() {
            bail!("metadata-only restore needs a v6 pool-sync drive");
        }
        valid_path(path)?;
        let destination = self.move_destination(path)?;
        if destination.depends_on.is_some() {
            bail!("{path} has local changes not yet uploaded; sync first");
        }
        let mut s = self.state.lock().unwrap();
        let mut next = s.clone();
        let id = next.commit(&destination, Some(content.clone()))?;
        next.save(&self.root)?;
        *s = next;
        drop(s);
        self.pins.lock().unwrap().insert(
            path.into(),
            super::virtual_drive::Revision::Cloud {
                id: id.clone(),
                content,
            },
        );
        Ok(id)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! A local v6 pool-sync drive without any cloud (publication fails).
    use super::*;
    pub(crate) fn v6_drive(root: &Path, events: BTreeMap<String, Event>) -> VirtualDrive {
        let mut drive = super::super::virtual_drive::fixture(root);
        drive.pool_sync_roots = vec!["crypt:history-test".into()];
        let mut state = drive.state.lock().unwrap();
        state.version = 6;
        state.published.extend(events.keys().cloned());
        state.ingest(events).unwrap();
        state.save(root).unwrap();
        drop(state);
        drive
    }
    /// What a successful sync does with queued deletions.
    pub(crate) fn commit_pending_deletions(drive: &VirtualDrive) {
        let pending = drive.state.lock().unwrap().pending.clone();
        for intent in pending.iter().filter(|i| i.spool.is_none()) {
            drive.commit_uploaded(intent, None).unwrap();
        }
    }
}
