//! Lookup, listing and namespace changes, with unsealed creates overlaid.
use super::core::{checked, lock, Attr, FsCore};
use super::error::{FsError, FsResult};
use super::identity::FileId;
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
    /// Every file path with its attributes.
    fn files(&self) -> FsResult<BTreeMap<String, Attr>> {
        let mut files = BTreeMap::new();
        for (path, revision) in self.drive.view()? {
            let id = lock(&self.ids)?.get(&path);
            files.insert(
                path,
                Attr {
                    id,
                    size: revision.size(),
                    directory: false,
                    tag: revision.id().into(),
                },
            );
        }
        for (path, (id, size, tag)) in self.unsealed()? {
            files.insert(
                path,
                Attr {
                    id: Some(id),
                    size,
                    directory: false,
                    tag,
                },
            );
        }
        Ok(files)
    }
    fn is_directory(&self, path: &str, files: &BTreeMap<String, Attr>) -> FsResult<bool> {
        if path.is_empty() {
            return Ok(true);
        }
        let prefix = format!("{path}/");
        Ok(files.keys().any(|p| p.starts_with(&prefix))
            || lock(&self.drive.state)?.directories.contains(path))
    }

    pub(crate) fn lookup(&self, path: &str) -> FsResult<Attr> {
        let files = self.files()?;
        if let Some(attr) = files.get(path) {
            return Ok(attr.clone());
        }
        if self.is_directory(path, &files)? {
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
        let files = self.files()?;
        if files.contains_key(directory) {
            return Err(FsError::NotDir);
        }
        if !self.is_directory(directory, &files)? {
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
        let mut entries = BTreeMap::new();
        for (path, attr) in &files {
            if let Some(relative) = path.strip_prefix(&prefix) {
                match relative.split_once('/') {
                    Some((child, _)) => entries.insert(child.to_owned(), folder.clone()),
                    None => entries.insert(relative.to_owned(), attr.clone()),
                };
            }
        }
        for name in lock(&self.drive.state)?.directories.iter() {
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
            self.drive.delete(path)?;
        }
        lock(&self.ids)?.unlink(path);
        Ok(())
    }

    /// Rename a file or directory. Unsealed writes of moved files are sealed
    /// first, because the drive moves acknowledged revisions only.
    pub(crate) fn rename(&self, from: &str, to: &str) -> FsResult<()> {
        checked(from)?;
        checked(to)?;
        let _namespace = self.exclusive()?;
        if from == to {
            return Ok(());
        }
        let source = self.lookup(from)?;
        if source.directory {
            let target = format!("{to}/");
            if self
                .unsealed()?
                .keys()
                .any(|p| p == to || p.starts_with(&target))
            {
                return Err(FsError::Exists);
            }
            let prefix = format!("{from}/");
            for (path, (file, _, _)) in self.unsealed()? {
                if path.starts_with(&prefix) {
                    self.seal_file(file)?;
                }
            }
            self.drive.rename_directory(from, to)?;
            lock(&self.ids)?.rename_directory(from, to);
            return Ok(());
        }
        if matches!(self.lookup(to), Ok(attr) if attr.directory) {
            return Err(FsError::IsDir);
        }
        if let Some(file) = source.id {
            self.seal_file(file)?;
        }
        self.drive.rename_file(from, to)?;
        lock(&self.ids)?.rename(from, to);
        Ok(())
    }

    pub(crate) fn statfs(&self) -> Option<(u64, Option<u64>)> {
        self.drive.quota()
    }
}
