//! Write intents: begin, seal, delete, directories and import.

use super::*;

impl VirtualDrive {
    #[cfg(test)]
    pub(crate) fn begin(&self, path: &str) -> Result<Intent> {
        valid_path(path)?;
        let visible = self.view()?.get(path).cloned();
        self.begin_observed(path, visible.as_ref())
    }
    pub(crate) fn begin_observed(&self, path: &str, visible: Option<&Revision>) -> Result<Intent> {
        self.begin_intent(path, visible)
    }
    pub(crate) fn begin_intent(&self, path: &str, visible: Option<&Revision>) -> Result<Intent> {
        valid_path(path)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let parents = if let Some(base) = s.bases.get(path) {
            base.clone()
        } else {
            match visible {
                Some(Revision::Cloud { id, .. }) => vec![id.clone()],
                _ => s.base(path)?,
            }
        };
        s.bases
            .entry(path.into())
            .or_insert_with(|| parents.clone());
        let event_path = parents
            .first()
            .and_then(|id| s.events.get(id))
            .map(|e| e.path.clone())
            .unwrap_or_else(|| path.into());
        // No trusted editor revision arrives with a generic DAV PUT. Never infer
        // a dependency on a newly arrived local/remote write that was not read.
        let depends_on = None;
        s.save(&self.root)?;
        let id = random_id()?;
        let dir = self.root.join("spool").join(&id);
        fs::create_dir(&dir)?;
        Ok(Intent {
            id: id.clone(),
            path: path.into(),
            event_path,
            parents,
            spool: Some(id),
            size: 0,
            hash: String::new(),
            depends_on,
        })
    }
    pub(super) fn prepare_seal(&self, mut intent: Intent) -> Result<Intent> {
        let path = self.spool_path(&intent);
        // Windows cannot flush through a read-only handle (os error 5).
        let file = crate::utils::open_for_sync(&path)
            .with_context(|| format!("seal: open spool {}", path.display()))?;
        crate::mount::crash::point("seal.before_fsync")?;
        file.sync_all()
            .with_context(|| format!("seal: flush spool {}", path.display()))?;
        intent.size = file.metadata()?.len();
        drop(file);
        intent.hash = crate::utils::hash_file_range(&path, 0, intent.size)
            .with_context(|| format!("seal: hash spool {}", path.display()))?;
        durable_json(&path.parent().unwrap().join("intent.json"), &intent)
            .context("seal: record intent")?;
        crate::mount::crash::point("seal.after_intent_record")?;
        #[cfg(unix)]
        {
            File::open(path.parent().unwrap())?.sync_all()?;
            File::open(self.root.join("spool"))?.sync_all()?;
        }
        Ok(intent)
    }
    pub(crate) fn seal(&self, intent: Intent) -> Result<()> {
        let intent = self.prepare_seal(intent)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut next = s.clone();
        next.pending.push(intent.clone());
        crate::mount::crash::point("seal.before_namespace_save")?;
        next.save(&self.root).context("seal: save namespace")?;
        crate::mount::crash::point("seal.after_namespace_save")?;
        *s = next;
        self.pins.lock().unwrap().insert(
            intent.path.clone(),
            Revision::Local {
                id: intent.id.clone(),
                path: self.spool_path(&intent),
                size: intent.size,
                _lease: self.local_lease(&intent.id),
            },
        );
        Ok(())
    }
    pub(in crate::mount) fn deletion_for(&self, path: &str, revision: &Revision) -> Result<Intent> {
        let mut intent = self.begin_observed(path, Some(revision))?;
        intent.spool = None;
        match revision {
            Revision::Local { id, .. } => {
                intent.depends_on = Some(id.clone());
                intent.parents.clear();
            }
            Revision::Cloud { id, .. } => {
                intent.parents = vec![id.clone()];
                intent.event_path = self
                    .state
                    .lock()
                    .unwrap()
                    .events
                    .get(id)
                    .context("missing selected revision")?
                    .path
                    .clone();
            }
        }
        Ok(intent)
    }
    /// Record an explicit (possibly empty) directory.
    pub(crate) fn create_directory(&self, path: &str) -> Result<()> {
        valid_path(path)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut next = s.clone();
        next.directories.insert(path.into());
        next.save(&self.root)?;
        *s = next;
        Ok(())
    }
    /// Remove an explicit directory. `Ok(false)` means it still has children.
    pub(crate) fn remove_directory(&self, path: &str) -> Result<bool> {
        let prefix = format!("{path}/");
        if self.view()?.keys().any(|n| n.starts_with(&prefix)) {
            return Ok(false);
        }
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        if s.directories.iter().any(|name| name.starts_with(&prefix)) {
            return Ok(false);
        }
        let mut next = s.clone();
        next.directories.remove(path);
        next.save(&self.root)?;
        *s = next;
        Ok(true)
    }
    pub(crate) fn delete(&self, path: &str) -> Result<()> {
        let revision = self
            .view()?
            .get(path)
            .cloned()
            .context("delete source missing")?;
        let intent = self.deletion_for(path, &revision)?;
        self.record_deletion(path, intent)
    }
    /// Durably queue a deletion intent for `path`.
    pub(in crate::mount) fn record_deletion(&self, path: &str, intent: Intent) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut next = s.clone();
        next.pending.push(intent);
        crate::mount::crash::point("delete.before_namespace_save")?;
        next.save(&self.root)?;
        *s = next;
        self.pins.lock().unwrap().remove(path);
        Ok(())
    }
    pub(crate) fn import(&self, source: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(&self.rclone, source)?;
        crate::manifest::validate_manifest(&manifest)?;
        // Import namespace without reading data. A whole-file hash is not in the
        // archive format, so use its authenticated content-root identity here.
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let event = Event {
            version: 1,
            device: s.device.clone(),
            worker: s.worker.clone(),
            path: manifest.original_name.clone(),
            parents: vec![],
            content: Some(Content {
                hash: manifest.content_root_blake3.clone(),
                size: manifest.original_size,
                manifest,
            }),
        };
        event.validate()?;
        let id = event.id()?;
        let mut next = s.clone();
        next.events.insert(id, event);
        next.save(&self.root)?;
        *s = next;
        Ok(())
    }
}
