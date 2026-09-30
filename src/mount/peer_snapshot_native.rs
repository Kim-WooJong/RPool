//! Native-frontend entry points for v7 private snapshots. A native frontend
//! seals every close, so renames routinely happen while intents are pending
//! (an editor's atomic save renames a fresh temp file over its target). The
//! plain v7 rename refuses any pending intent; here:
//! - a never-uploaded ("fresh") chain is moved by retargeting its pending
//!   paths, and a synced file's pending edits move with its `NameOp`;
//! - a fresh temp renamed over an existing file becomes that file's next
//!   revision, and the temp's other never-uploaded intents are dropped;
//! - chains whose upload plan already started are `Busy`; anything v7 cannot
//!   express yet is `CrossDevice`, so tools fall back to copy and delete.
//!
//! Ancestry is final before originals are captured, so the captured original
//! is the revision the edit really descends from.
use super::{hash, model, name_heads, validate_names, Io, NameOp, State};
use crate::mount::namespace::{durable_json, random_id, valid_path, Intent, Namespace};
use crate::mount::native_ancestry::{Ancestry, Busy, CrossDevice};
use crate::mount::virtual_drive::{Revision as DriveRevision, VirtualDrive};
use crate::prelude::*;

fn under(path: &str, root: &str, directory: bool) -> bool {
    path == root || (directory && path.starts_with(&format!("{root}/")))
}
fn moved(path: &str, from: &str, to: &str) -> String {
    format!("{to}{}", &path[from.len()..])
}

impl VirtualDrive {
    pub(crate) fn begin_snapshot_based(
        &self,
        path: &str,
        visible: Option<&DriveRevision>,
        ancestry: &Ancestry,
    ) -> Result<Intent> {
        let store = self.snapshot_store()?;
        self.begin_snapshot_based_with(path, visible, ancestry, &store)
    }
    pub(super) fn begin_snapshot_based_with(
        &self,
        path: &str,
        visible: Option<&DriveRevision>,
        ancestry: &Ancestry,
        store: &dyn Io,
    ) -> Result<Intent> {
        let mut intent = self.begin_intent(path, visible)?;
        self.apply_ancestry(&mut intent, ancestry)?;
        self.capture_originals(&intent, store)?;
        Ok(intent)
    }

    pub(crate) fn delete_snapshot_based(&self, path: &str, ancestry: &Ancestry) -> Result<()> {
        let store = self.snapshot_store()?;
        self.delete_snapshot_based_with(path, ancestry, &store)
    }
    pub(super) fn delete_snapshot_based_with(
        &self,
        path: &str,
        ancestry: &Ancestry,
        store: &dyn Io,
    ) -> Result<()> {
        let revision = self
            .view()?
            .get(path)
            .cloned()
            .context("delete source missing")?;
        let ancestry = match ancestry {
            Ancestry::Default => Ancestry::of(&revision),
            other => other.clone(),
        };
        let mut intent = self.begin_intent(path, Some(&revision))?;
        intent.spool = None;
        self.apply_ancestry(&mut intent, &ancestry)?;
        self.capture_originals(&intent, store)?;
        self.record_deletion(path, intent)
    }

    /// Renames for the native frontend. `replace` is the destination's
    /// ancestry (what the editor last read) when `to` is an existing file.
    pub(crate) fn rename_native(
        &self,
        from: &str,
        to: &str,
        directory: bool,
        replace: Option<&Ancestry>,
    ) -> Result<()> {
        let store = self.snapshot_store()?;
        self.rename_native_with(from, to, directory, replace, &store)
    }
    pub(super) fn rename_native_with(
        &self,
        from: &str,
        to: &str,
        directory: bool,
        replace: Option<&Ancestry>,
        store: &dyn Io,
    ) -> Result<()> {
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        if directory && (to.starts_with(&format!("{from}/")) || from.starts_with(&format!("{to}/")))
        {
            bail!("overlapping move");
        }
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync gate poisoned"))?;
        let view = self.view()?;
        if view.keys().any(|p| under(p, to, directory)) {
            if directory {
                bail!("move destination exists");
            }
            return self.replace_fresh(from, to, replace, &view, store);
        }
        self.move_paths(from, to, directory)
    }

    fn planned(&self, intent: &Intent) -> bool {
        self.root
            .join("spool")
            .join(&intent.id)
            .join("snapshot-plan.json")
            .exists()
    }

    /// Pending intents at each moved path, in namespace order.
    fn chains(pending: &[Intent], from: &str, directory: bool) -> BTreeMap<String, Vec<Intent>> {
        let mut chains: BTreeMap<String, Vec<Intent>> = BTreeMap::new();
        for intent in pending.iter().filter(|i| under(&i.path, from, directory)) {
            chains
                .entry(intent.path.clone())
                .or_default()
                .push(intent.clone());
        }
        chains
    }

    /// Refuses chains that are uploading, contain deletions, or that other
    /// pending intents depend on.
    fn movable(&self, pending: &[Intent], chains: &BTreeMap<String, Vec<Intent>>) -> Result<()> {
        let members: BTreeSet<&String> = chains.values().flatten().map(|i| &i.id).collect();
        for chain in chains.values() {
            if chain.iter().any(|i| self.planned(i)) {
                return Err(Busy("the file is being uploaded; retry after this sync").into());
            }
            if chain.iter().any(|i| i.spool.is_none()) {
                return Err(CrossDevice("a pending delete is under the moved path").into());
            }
        }
        if pending.iter().any(|i| {
            !members.contains(&i.id) && i.depends_on.as_ref().is_some_and(|d| members.contains(d))
        }) {
            return Err(CrossDevice("another pending edit continues the moved file").into());
        }
        Ok(())
    }

    /// The synced file identity a non-fresh chain edits.
    fn chain_file(&self, state: &State, namespace: &Namespace, root: &Intent) -> Result<String> {
        let events: Vec<String> = match &root.depends_on {
            Some(dependency) => match namespace.committed_intents.get(dependency) {
                Some(event) => vec![event.clone()],
                None => return Err(CrossDevice("the edit continues another pending file").into()),
            },
            None => root.parents.clone(),
        };
        let mut files = events
            .iter()
            .map(|e| state.mappings.get(e).map(|t| t.file.clone()));
        let first = files
            .next()
            .flatten()
            .ok_or(CrossDevice("unknown file identity"))?;
        if files.any(|f| f.as_ref() != Some(&first)) {
            return Err(CrossDevice("the edit spans several files").into());
        }
        Ok(first)
    }

    fn single_head(&self, state: &State, analysis: &model::Analysis, file: &str) -> Result<()> {
        if analysis.merged.get(file).is_none_or(|m| m.heads.len() != 1)
            || name_heads(state, file).len() != 1
        {
            return Err(CrossDevice("resolve the conflict before moving this file").into());
        }
        Ok(())
    }

    fn move_paths(&self, from: &str, to: &str, directory: bool) -> Result<()> {
        let namespace = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?
            .clone();
        let chains = Self::chains(&namespace.pending, from, directory);
        self.movable(&namespace.pending, &chains)?;
        let mut state = self.snapshot_state()?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let mut entries = BTreeMap::new();
        let mut parents = BTreeMap::new();
        let mut retarget = BTreeMap::new();
        for (path, chain) in &chains {
            let destination = moved(path, from, to);
            let root = &chain[0];
            if !(root.parents.is_empty() && root.depends_on.is_none()) {
                let file = self.chain_file(&state, &namespace, root)?;
                self.single_head(&state, &analysis, &file)?;
                parents.insert(file.clone(), name_heads(&state, &file));
                entries.insert(file, destination.clone());
            }
            for intent in chain {
                retarget.insert(intent.id.clone(), destination.clone());
            }
        }
        for (path, event) in &namespace.snapshot_view {
            if !under(path, from, directory) || chains.contains_key(path) {
                continue;
            }
            if namespace
                .events
                .get(event)
                .is_none_or(|e| e.content.is_none())
            {
                continue;
            }
            let target = state
                .mappings
                .get(event)
                .context("missing move identity")?
                .clone();
            self.single_head(&state, &analysis, &target.file)?;
            parents.insert(target.file.clone(), name_heads(&state, &target.file));
            if entries.insert(target.file, moved(path, from, to)).is_some() {
                return Err(CrossDevice("the file is visible under several names").into());
            }
        }
        if entries.is_empty() && retarget.is_empty() {
            bail!("move source not found");
        }
        let named = !entries.is_empty();
        if named {
            let op = NameOp {
                nonce: random_id()?,
                entries,
                parents,
            };
            state.names.insert(hash(&op)?, op);
            validate_names(&state)?;
            // Acknowledgement: the new names of synced files.
            self.save_snapshot_state(&state)?;
        }
        for (id, destination) in &retarget {
            let record = self.root.join("spool").join(id).join("intent.json");
            if record.exists() {
                let mut intent: Intent = crate::utils::read_json(&record)?;
                intent.path = destination.clone();
                durable_json(&record, &intent)?;
            }
        }
        {
            let mut current = self
                .state
                .lock()
                .map_err(|_| anyhow!("namespace lock poisoned"))?;
            let mut next = current.clone();
            for intent in next.pending.iter_mut() {
                if let Some(destination) = retarget.get(&intent.id) {
                    intent.path = destination.clone();
                }
            }
            if directory {
                next.directories = next
                    .directories
                    .iter()
                    .map(|d| {
                        if under(d, from, true) {
                            moved(d, from, to)
                        } else {
                            d.clone()
                        }
                    })
                    .collect();
            }
            let bases: Vec<String> = next
                .bases
                .keys()
                .filter(|p| under(p, from, directory))
                .cloned()
                .collect();
            for path in bases {
                if let Some(base) = next.bases.remove(&path) {
                    next.bases.insert(moved(&path, from, to), base);
                }
            }
            next.save(&self.root)?;
            *current = next;
        }
        self.move_pins(from, to, directory);
        if named {
            let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
            self.materialize_snapshots(&mut state, &analysis)?;
        }
        Ok(())
    }

    fn move_pins(&self, from: &str, to: &str, directory: bool) {
        if let Ok(mut pins) = self.pins.lock() {
            let keys: Vec<String> = pins
                .keys()
                .filter(|p| under(p, from, directory))
                .cloned()
                .collect();
            for key in keys {
                if let Some(pin) = pins.remove(&key) {
                    pins.insert(moved(&key, from, to), pin);
                }
            }
        }
        if let Ok(mut pins) = self.peer_read_pins.lock() {
            let keys: Vec<String> = pins
                .keys()
                .filter(|p| under(p, from, directory))
                .cloned()
                .collect();
            for key in keys {
                if let Some(pin) = pins.remove(&key) {
                    pins.insert(moved(&key, from, to), pin);
                }
            }
        }
    }

    /// A fresh file renamed over an existing file (an atomic save): its last
    /// intent becomes the destination's next revision, descending from what
    /// the editor read (`replace`) or from the visible destination.
    fn replace_fresh(
        &self,
        from: &str,
        to: &str,
        replace: Option<&Ancestry>,
        view: &BTreeMap<String, DriveRevision>,
        store: &dyn Io,
    ) -> Result<()> {
        let namespace = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?
            .clone();
        let chains = Self::chains(&namespace.pending, from, false);
        let Some(chain) = chains.get(from) else {
            return Err(CrossDevice("renaming a synced file over another file").into());
        };
        self.movable(&namespace.pending, &chains)?;
        let root = &chain[0];
        if !(root.parents.is_empty() && root.depends_on.is_none()) {
            return Err(CrossDevice("renaming an edited synced file over another file").into());
        }
        let last = chain.last().context("empty chain")?;
        let target = view.get(to).context("rename destination vanished")?;
        let ancestry = match replace {
            Some(a) if *a != Ancestry::Default => a.clone(),
            _ => Ancestry::of(target),
        };
        let mut rewritten = last.clone();
        rewritten.path = to.into();
        rewritten.event_path = to.into();
        rewritten.depends_on = None;
        rewritten.parents.clear();
        self.apply_ancestry(&mut rewritten, &ancestry)?;
        if rewritten.parents.is_empty() && rewritten.depends_on.is_none() {
            return Err(CrossDevice("the destination revision is not known locally").into());
        }
        self.capture_originals(&rewritten, store)?;
        durable_json(
            &self
                .root
                .join("spool")
                .join(&rewritten.id)
                .join("intent.json"),
            &rewritten,
        )?;
        let dropped: Vec<String> = chain
            .iter()
            .filter(|i| i.id != rewritten.id)
            .map(|i| i.id.clone())
            .collect();
        {
            let mut current = self
                .state
                .lock()
                .map_err(|_| anyhow!("namespace lock poisoned"))?;
            let mut next = current.clone();
            let members: BTreeSet<&String> = chain.iter().map(|i| &i.id).collect();
            next.pending.retain(|i| !members.contains(&i.id));
            // Last, so every intent it depends on is committed first.
            next.pending.push(rewritten);
            next.bases.remove(from);
            next.save(&self.root)?;
            *current = next;
        }
        // Never-uploaded, superseded images of the temp file.
        for id in dropped {
            let _ = fs::remove_dir_all(self.root.join("spool").join(id));
        }
        if let Ok(mut pins) = self.pins.lock() {
            pins.remove(from);
        }
        Ok(())
    }
}
