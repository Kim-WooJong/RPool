//! What drive history (`crate::drive_history`) uses from the mount: opening
//! a pool-sync workspace, its v6 events / v7 snapshot export, and writing a
//! restored revision. Every write is an ordinary new revision of the drive
//! (the same records a mounted PC publishes); nothing here removes history.
pub(crate) use super::namespace::valid_path;
pub(crate) use super::peer_snapshot::history::Export;
#[cfg(test)]
pub(crate) use super::peer_snapshot::history::{ExportName, ExportRevision, ExportSnapshot};
pub(crate) use super::shared_model::{peer_path, Content, Event};
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

/// Opens an existing, unmounted v6/v7 pool-sync workspace of `pool` (takes
/// its lock, like a mount; fails while it is mounted).
pub(crate) fn open_workspace(rclone: &str, pool: &str, root: &Path) -> Result<VirtualDrive> {
    let binding: Binding = crate::utils::read_json(&root.join("virtual.json"))
        .with_context(|| format!("not an RPool online-drive workspace: {}", root.display()))?;
    if binding.pool != pool {
        bail!("workspace belongs to pool {}, not {pool}", binding.pool);
    }
    let v7 = match binding.version {
        6 => false,
        7 => true,
        _ => bail!("trash/versions/rollback need an automatic pool-sync (v6/v7) workspace"),
    };
    let worker: String = crate::utils::read_json(&root.join("pool-worker.json"))
        .context("workspace has no pool worker label; mount it once first")?;
    let mut drive = VirtualDrive::open(
        rclone,
        pool,
        root,
        &worker,
        None,
        CACHE_BYTES,
        false,
        true,
        v7,
    )?;
    drive.pool_history_limit = super::pool_sync::Config::load(&drive.root, None)?.history_limit;
    Ok(drive)
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
    let v7 = generation.v7;
    let mut drive = match &generation.epoch {
        Some(epoch) => VirtualDrive::open_with_epoch(
            rclone,
            pool,
            root,
            worker,
            None,
            CACHE_BYTES,
            false,
            true,
            v7,
            epoch,
        )?,
        None => VirtualDrive::open(
            rclone,
            pool,
            root,
            worker,
            None,
            CACHE_BYTES,
            false,
            true,
            v7,
        )?,
    };
    if v7 {
        // The history limit is part of the v7 policy identity: adopt the pool's.
        if !drive.pull_snapshots_adopting_policy()? {
            bail!("the pool has no v7 drive records");
        }
        // Saved so a later `mount --sync-only` of this scratch uses the same policy.
        super::pool_sync::Config::load(&drive.root, Some(drive.pool_history_limit))?;
    } else {
        drive.pull()?;
    }
    durable_worker(&drive.root, worker)?;
    Ok(drive)
}

fn durable_worker(root: &Path, worker: &str) -> Result<()> {
    super::namespace::durable_json(&root.join("pool-worker.json"), &worker)
}

pub(crate) fn is_v7(drive: &VirtualDrive) -> bool {
    drive.peer_retention
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
    /// does), so v7 captures the current original, not an older read.
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
        if self.peer_retention || self.bounded_shared || self.pool_sync_roots.is_empty() {
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

    /// Writes the bytes of `manifest` as a new local version of `path`
    /// (v7: the next sync uploads them as this workspace's own payload).
    pub(crate) fn history_put_bytes(&self, path: &str, manifest: &Manifest) -> Result<()> {
        valid_path(path)?;
        let visible = self.view()?.get(path).cloned();
        let intent = self.begin_observed(path, visible.as_ref())?;
        let target = self.spool_path(&intent);
        let result = (|| {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            let reader = crate::storage::reader::StorageReader::rclone(&self.rclone);
            let mut offset = 0;
            while offset < manifest.original_size {
                let bytes = self.cache.read(
                    &reader,
                    manifest,
                    offset,
                    4 << 20,
                    self.policy.workers,
                    self.policy.retries,
                )?;
                if bytes.is_empty() {
                    bail!("short read of the restored version");
                }
                self.write_spool_bytes(&mut output, &bytes)?;
                offset += bytes.len() as u64;
            }
            output.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = self.discard_unsealed(&intent);
            return Err(
                error.context("could not read the version's data (it may have been collected)")
            );
        }
        self.seal(intent)
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
