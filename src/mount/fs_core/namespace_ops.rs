//! Lookup, listing and namespace changes, with unsealed creates overlaid.
use super::core::{checked, lock, Attr, FsCore};
use super::error::{FsError, FsResult};
use super::identity::FileId;
use crate::mount::virtual_drive::VisibleView;
use crate::prelude::*;

impl FsCore {
    /// Unsealed generations of linked files, by path.
    fn unsealed(&self) -> FsResult<BTreeMap<String, (FileId, u64, String)>> {
        let files: Vec<FileId> = lock(&self.slots)?.keys().copied().collect();
        let mut result = BTreeMap::new();
        for file in files {
            let Some((size, tag)) = self.unsealed_size(file)? else {
                continue;
            };
            if let Some(path) = lock(&self.ids)?.path(file) {
                result.insert(path, (file, size, tag));
            }
        }
        Ok(result)
    }
    /// The file at `path` as this core sees it: an unsealed generation, else
    /// the visible revision.
    fn file_attr(
        &self,
        view: &VisibleView,
        unsealed: &BTreeMap<String, (FileId, u64, String)>,
        path: &str,
    ) -> FsResult<Option<Attr>> {
        if let Some((id, size, tag)) = unsealed.get(path) {
            return Ok(Some(Attr {
                id: Some(*id),
                size: *size,
                directory: false,
                tag: tag.clone(),
            }));
        }
        let Some((tag, size)) = view.file(path) else {
            return Ok(None);
        };
        Ok(Some(Attr {
            id: lock(&self.ids)?.get(path),
            size,
            directory: false,
            tag: tag.into(),
        }))
    }
    fn is_directory(
        &self,
        view: &VisibleView,
        unsealed: &BTreeMap<String, (FileId, u64, String)>,
        path: &str,
    ) -> bool {
        if path.is_empty() {
            return true;
        }
        let prefix = format!("{path}/");
        view.has_descendant(&prefix)
            || unsealed.keys().any(|p| p.starts_with(&prefix))
            || view.directories().contains(path)
    }

    pub(crate) fn lookup(&self, path: &str) -> FsResult<Attr> {
        let view = self.drive.visible()?;
        let unsealed = self.unsealed()?;
        if let Some(attr) = self.file_attr(&view, &unsealed, path)? {
            return Ok(attr);
        }
        if self.is_directory(&view, &unsealed, path) {
            return Ok(Attr {
                id: None,
                size: 0,
                directory: true,
                tag: String::new(),
            });
        }
        Err(FsError::NotFound)
    }

    pub(crate) fn readdir(&self, directory: &str) -> FsResult<Vec<(String, Attr)>> {
        let view = self.drive.visible()?;
        let unsealed = self.unsealed()?;
        if self.file_attr(&view, &unsealed, directory)?.is_some() {
            return Err(FsError::NotDir);
        }
        if !self.is_directory(&view, &unsealed, directory) {
            return Err(FsError::NotFound);
        }
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let folder = Attr {
            id: None,
            size: 0,
            directory: true,
            tag: String::new(),
        };
        // As in a full listing, a subdirectory wins over a same-named file.
        let mut entries = BTreeMap::new();
        for (child, file) in view.children(&prefix) {
            let attr = match file {
                None => folder.clone(),
                Some(_) => {
                    let path = format!("{prefix}{child}");
                    match self.file_attr(&view, &unsealed, &path)? {
                        Some(attr) => attr,
                        None => continue,
                    }
                }
            };
            entries.insert(child, attr);
        }
        for (path, (id, size, tag)) in &unsealed {
            let Some(relative) = path.strip_prefix(&prefix) else {
                continue;
            };
            match relative.split_once('/') {
                Some((child, _)) => {
                    entries.insert(child.to_owned(), folder.clone());
                }
                None => {
                    if !entries.get(relative).is_some_and(|a| a.directory) {
                        let attr = Attr {
                            id: Some(*id),
                            size: *size,
                            directory: false,
                            tag: tag.clone(),
                        };
                        entries.insert(relative.to_owned(), attr);
                    }
                }
            }
        }
        for name in view.directories() {
            if let Some(relative) = name.strip_prefix(&prefix) {
                let child = relative.split('/').next().unwrap_or(relative);
                if !child.is_empty() {
                    entries
                        .entry(child.to_owned())
                        .or_insert_with(|| folder.clone());
                }
            }
        }
        Ok(entries.into_iter().collect())
    }

    pub(crate) fn mkdir(&self, path: &str) -> FsResult<()> {
        checked(path)?;
        let _namespace = self.exclusive()?;
        if self.lookup(path).is_ok() {
            return Err(FsError::Exists);
        }
        Ok(self.drive.create_directory(path)?)
    }

    pub(crate) fn rmdir(&self, path: &str) -> FsResult<()> {
        checked(path)?;
        let _namespace = self.exclusive()?;
        if !self.lookup(path)?.directory {
            return Err(FsError::NotDir);
        }
        let prefix = format!("{path}/");
        if self.unsealed()?.keys().any(|p| p.starts_with(&prefix)) {
            return Err(FsError::NotEmpty);
        }
        match self.drive.remove_directory(path)? {
            true => Ok(()),
            false => Err(FsError::NotEmpty),
        }
    }

    /// Unlink a file. Open handles keep working; its unsealed writes are never
    /// acknowledged.
    pub(crate) fn delete(&self, path: &str) -> FsResult<()> {
        checked(path)?;
        let _namespace = self.exclusive()?;
        let unsealed = match lock(&self.ids)?.get(path) {
            Some(file) => self.unsealed_size(file)?.is_some(),
            None => false,
        };
        let visible = self.visible(path)?.is_some();
        if !visible && !unsealed {
            return Err(match self.lookup(path) {
                Ok(attr) if attr.directory => FsError::IsDir,
                _ => FsError::NotFound,
            });
        }
        if visible {
            match (self.peer, lock(&self.ids)?.get(path)) {
                (true, Some(file)) => {
                    let ancestry = self.read_ancestry(file, path)?;
                    self.drive.delete_based(path, &ancestry)?;
                }
                _ => self.drive.delete(path)?,
            }
        }
        lock(&self.ids)?.unlink(path);
        Ok(())
    }

    /// Rename a file or directory. Unsealed writes of moved files are sealed
    /// first, because the drive moves acknowledged revisions only.
    pub(crate) fn rename(&self, from: &str, to: &str) -> FsResult<()> {
        checked(from)?;
        checked(to)?;
        // Hash moved unsealed files before taking the exclusive lock, so the
        // seals below only record intents.
        let prefix = format!("{from}/");
        let mut unsealed = BTreeSet::new();
        for (path, (file, _, _)) in self.unsealed()? {
            if path == from || path.starts_with(&prefix) {
                self.prehash(file)?;
                unsealed.insert(path);
            }
        }
        // Link (or, without hard links, copy) the moved sealed images now too,
        // so the MOVE under the lock only renames them into place.
        let mut staged = if from == to {
            Default::default()
        } else {
            self.drive.stage_move(from, &unsealed)
        };
        let _namespace = self.exclusive()?;
        let source = self.lookup(from)?;
        if from == to {
            return Ok(());
        }
        if source.directory {
            let target = format!("{to}/");
            if self
                .unsealed()?
                .keys()
                .any(|p| p == to || p.starts_with(&target))
            {
                return Err(FsError::Exists);
            }
            for (path, (file, _, _)) in self.unsealed()? {
                if path.starts_with(&prefix) {
                    self.seal_file(file)?;
                }
            }
            self.drive.rename_directory_staged(from, to, &mut staged)?;
            lock(&self.ids)?.rename_directory(from, to);
            return Ok(());
        }
        if matches!(self.lookup(to), Ok(attr) if attr.directory) {
            return Err(FsError::IsDir);
        }
        if let Some(file) = source.id {
            self.seal_file(file)?;
        }
        self.drive.rename_file_staged(from, to, &mut staged)?;
        lock(&self.ids)?.rename(from, to);
        // Handles follow the moved file; its new revision is the moved copy.
        if let (Some(file), Some(moved)) = (source.id, self.visible(to)?) {
            lock(&self.observed)?.insert(file, moved.clone());
            lock(&self.handles)?.rebase(file, Some(moved));
        }
        Ok(())
    }

    pub(crate) fn statfs(&self) -> Option<(u64, Option<u64>)> {
        self.drive.quota()
    }
}
