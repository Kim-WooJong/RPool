//! File and directory renames inside the virtual namespace.

use super::*;

impl VirtualDrive {
    /// MOVE expresses an explicit namespace operation, unlike an unconditioned PUT.
    /// A missing destination must descend from its current deletion, not an old editor base.
    pub(in crate::mount) fn move_destination(&self, path: &str) -> Result<Intent> {
        let visible = self.view()?.get(path).cloned();
        let mut intent = self.begin_observed(path, visible.as_ref())?;
        let state = self.state.lock().unwrap();
        if self.bounded_shared {
            return Ok(intent);
        }
        if let Some(previous) = state.pending.iter().rev().find(|i| i.path == path) {
            intent.depends_on = Some(previous.id.clone());
            intent.event_path = previous.event_path.clone();
            intent.parents.clear();
        } else if let Some(Revision::Cloud { id, .. }) = visible {
            intent.parents = vec![id.clone()];
            intent.event_path = state
                .events
                .get(&id)
                .context("move destination revision missing")?
                .path
                .clone();
        } else {
            let referenced: BTreeSet<_> = state
                .events
                .values()
                .flat_map(|event| event.parents.iter())
                .collect();
            intent.parents = state
                .events
                .iter()
                .filter(|(id, event)| event.path == path && !referenced.contains(id))
                .map(|(id, _)| id.clone())
                .collect();
            intent.event_path = path.into();
        }
        Ok(intent)
    }
    pub(super) fn copy_revision_to_spool(&self, revision: &Revision, target: &Path) -> Result<()> {
        if let Revision::Local { path, .. } = revision {
            return self.copy_to_spool(path, target);
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let mut offset = 0;
        while offset < revision.size() {
            let bytes = self.read(revision, offset, 1024 * 1024)?;
            if bytes.is_empty() {
                bail!("short cloud read during MOVE");
            }
            self.write_spool_bytes(&mut output, &bytes)?;
            offset += bytes.len() as u64;
        }
        output.sync_all()?;
        Ok(())
    }
    pub(crate) fn rename_file(&self, from: &str, to: &str) -> Result<()> {
        if self.peer_retention {
            return self.rename_snapshot(from, to, false);
        }
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        let revision = self
            .view()?
            .get(from)
            .cloned()
            .context("rename source missing")?;
        let mut destination = self.move_destination(to)?;
        let mut deletion = self.deletion_for(from, &revision)?;
        if self.bounded_shared {
            deletion.depends_on = Some(destination.id.clone());
        }
        match revision {
            Revision::Cloud { content, .. } if !self.bounded_shared => {
                let mut s = self.state.lock().unwrap();
                let mut next = s.clone();
                if let Some(dep) = &destination.depends_on {
                    // Preserve cloud bytes without hydration while waiting on earlier local intents.
                    // An explicit synchronous commit of the already committed dependency is safe.
                    if !next.committed_intents.contains_key(dep) {
                        bail!("destination has pending local work; sync before moving this cloud revision");
                    }
                }
                let id = next.commit(&destination, Some(content.clone()))?;
                next.commit(&deletion, None)?;
                crate::mount::crash::point("rename.before_namespace_save")?;
                next.save(&self.root)?;
                *s = next;
                let mut pins = self.pins.lock().unwrap();
                pins.remove(from);
                pins.insert(to.into(), Revision::Cloud { id, content });
            }
            revision => {
                let size = revision.size();
                let output = self.spool_path(&destination);
                self.copy_revision_to_spool(&revision, &output)?;
                crate::utils::sync_file(&output)?;
                destination.size = size;
                destination.hash = crate::utils::hash_file_range(&output, 0, size)?;
                destination = self.prepare_seal(destination)?;
                let mut s = self.state.lock().unwrap();
                let mut next = s.clone();
                next.pending.push(destination.clone());
                next.pending.push(deletion);
                crate::mount::crash::point("rename.before_namespace_save")?;
                next.save(&self.root)?;
                *s = next;
                let mut pins = self.pins.lock().unwrap();
                pins.remove(from);
                pins.insert(
                    to.into(),
                    Revision::Local {
                        id: destination.id.clone(),
                        path: self.spool_path(&destination),
                        size,
                        _lease: self.local_lease(&destination.id),
                    },
                );
            }
        }
        Ok(())
    }
    pub(crate) fn rename_directory(&self, from: &str, to: &str) -> Result<()> {
        if self.peer_retention {
            return self.rename_snapshot(from, to, true);
        }
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(&format!("{from}/")) || from.starts_with(&format!("{to}/")) {
            bail!("overlapping directory move");
        }
        let view = self.view()?;
        let prefix = format!("{from}/");
        if view
            .keys()
            .any(|p| p == to || p.starts_with(&format!("{to}/")))
        {
            bail!("directory move destination exists");
        }
        let mut changes = vec![];
        for (name, revision) in view.iter().filter(|(name, _)| name.starts_with(&prefix)) {
            let target = format!("{to}/{}", &name[prefix.len()..]);
            let mut destination = self.move_destination(&target)?;
            let mut deletion = self.deletion_for(name, revision)?;
            if self.bounded_shared {
                deletion.depends_on = Some(destination.id.clone());
            }
            let content = match revision {
                Revision::Cloud { content, .. } if !self.bounded_shared => Some(content.clone()),
                revision => {
                    self.copy_revision_to_spool(revision, &self.spool_path(&destination))?;
                    destination = self.prepare_seal(destination)?;
                    None
                }
            };
            changes.push((name.clone(), destination, deletion, content));
        }
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        let directories: Vec<_> = next
            .directories
            .iter()
            .filter(|p| p.as_str() == from || p.starts_with(&prefix))
            .cloned()
            .collect();
        for directory in directories {
            next.directories.remove(&directory);
            next.directories
                .insert(format!("{to}{}", &directory[from.len()..]));
        }
        let mut new_pins = vec![];
        for (name, destination, deletion, content) in changes {
            let revision = if let Some(content) = content {
                let id = next.commit(&destination, Some(content.clone()))?;
                // Commit deletion only if its dependency is already committed.
                if deletion.depends_on.is_some() {
                    next.pending.push(deletion);
                } else {
                    next.commit(&deletion, None)?;
                }
                Revision::Cloud { id, content }
            } else {
                next.pending.push(destination.clone());
                next.pending.push(deletion);
                Revision::Local {
                    id: destination.id.clone(),
                    path: self.spool_path(&destination),
                    size: destination.size,
                    _lease: self.local_lease(&destination.id),
                }
            };
            new_pins.push((name, destination.path, revision));
        }
        next.save(&self.root)?;
        *state = next;
        let mut pins = self.pins.lock().unwrap();
        for (old, new, revision) in new_pins {
            pins.remove(&old);
            pins.insert(new, revision);
        }
        Ok(())
    }
}
