use super::*;
use crate::mount::capacity::CapacityStatus;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

impl Workspace {
    pub(crate) fn capacity_status(&self) -> Result<CapacityStatus> {
        let admin = RcloneAdmin::inherited(&self.rclone);
        let mut status = CapacityStatus::inspect(&admin, &self.catalog.policy)?;
        let catalog = admin.catalog()?;
        let mut archives = BTreeMap::new();
        let mut source_policy = self.catalog.policy.clone();
        for item in fs::read_dir(self.metadata.join("archives"))? {
            let path = item?.path();
            require_regular(&path)?;
            let manifest: Manifest = read_json(&path)?;
            crate::manifest::validate_manifest(&manifest)?;
            for shard in &manifest.shards {
                if !source_policy.remotes.contains(&shard.remote) {
                    source_policy.remotes.push(shard.remote.clone());
                }
            }
            archives.insert(
                path.file_name()
                    .context("archive filename missing")?
                    .to_string_lossy()
                    .into_owned(),
                manifest,
            );
        }
        // Imported archives may use quota-known targets outside the upload pool;
        // absence from the pool alone must not mark those archives for migration.
        let sources = if source_policy.remotes == self.catalog.policy.remotes {
            status.clone()
        } else {
            CapacityStatus::inspect(&admin, &source_policy)?
        };
        for remote in &sources.eligible {
            status
                .known_archive_targets
                .insert(catalog.placement_target(remote)?);
        }
        for excluded in sources.excluded {
            if !status.excluded.iter().any(|e| e.remote == excluded.remote) {
                status.excluded.push(excluded);
            }
        }
        let affected = |manifest: &Manifest| {
            manifest.shards.iter().any(|s| {
                catalog.placement_target(&s.object).map_or(true, |target| {
                    !status.known_archive_targets.contains(&target)
                })
            })
        };
        status.affected_active = self
            .catalog
            .entries
            .values()
            .filter(|e| !e.deleted)
            .map(|e| archives.get(&e.manifest).context("active archive missing"))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|m| affected(m))
            .count();
        status.retained_archives = archives.values().filter(|m| affected(m)).count();
        // Metadata-only traversal: don't hash the entire local replica for a meter.
        fn usage(dir: &Path) -> Result<u64> {
            require_directory(dir)?;
            let mut total = 0u64;
            for item in fs::read_dir(dir)? {
                let path = item?.path();
                let meta = fs::symlink_metadata(&path)?;
                if meta.file_type().is_symlink() || is_reparse(&meta) {
                    bail!("unsupported workspace link");
                }
                let bytes = if meta.is_dir() {
                    usage(&path)?
                } else {
                    require_regular(&path)?;
                    meta.len()
                };
                total = total
                    .checked_add(bytes)
                    .context("workspace usage overflow")?;
            }
            Ok(total)
        }
        status.logical_used = usage(&self.files)?;
        status.logical_ceiling_estimate = status
            .logical_used
            .saturating_add(status.additional_estimate);
        Ok(status)
    }

    pub(crate) fn migrate_excluded(&mut self) -> Result<usize> {
        if !self.shared_materialization_safe()? {
            bail!("Unmount and drain the original VFS cache before migrating archives");
        }
        let status = self.capacity_status()?;
        if status.eligible.is_empty() {
            bail!("No eligible destination for migration");
        }
        let admin = RcloneAdmin::inherited(&self.rclone);
        let catalog = admin.catalog()?;
        let allowed = status.known_archive_targets;
        let rclone = self.rclone.clone();
        let policy = self.catalog.policy.clone();
        let pool = self.catalog.pool.clone();
        let moved = self.migrate_with(
            |manifest| {
                manifest.shards.iter().any(|s| {
                    !catalog
                        .placement_target(&s.object)
                        .is_ok_and(|r| allowed.contains(&r))
                })
            },
            |source, output| {
                crate::commands::get(
                    &rclone,
                    &source.to_string_lossy(),
                    output,
                    policy.workers,
                    policy.retries,
                )
            },
            |source, id| upload_eligible(&rclone, &policy, &pool, source, id),
            |path| crate::commands::verify(&rclone, &path.to_string_lossy(), true, policy.workers),
            true,
        )?;
        self.sync_shared(false)?;
        Ok(moved)
    }

    fn migrate_with(
        &mut self,
        affected: impl Fn(&Manifest) -> bool,
        mut restore: impl FnMut(&Path, &Path) -> Result<()>,
        mut upload: impl FnMut(&Path, &str) -> Result<Manifest>,
        mut verify: impl FnMut(&Path) -> Result<()>,
        index: bool,
    ) -> Result<usize> {
        if !self.shared_materialization_safe()? {
            bail!("Unmount and drain VFS cache before migration");
        }
        let names: Vec<_> = self
            .catalog
            .entries
            .iter()
            .filter(|(_, e)| !e.deleted)
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect();
        let mut moved = 0;
        for (name, entry) in names {
            let old_path = self.metadata.join("archives").join(&entry.manifest);
            let old: Manifest = read_json(&old_path)?;
            crate::manifest::validate_manifest(&old)?;
            if !affected(&old) {
                continue;
            }
            let dir = self
                .metadata
                .join("transactions")
                .join(format!("migrate-{}", entry.manifest));
            fs::create_dir_all(&dir)?;
            require_directory(&dir)?;
            let id_path = dir.join("id.json");
            let id: String = if id_path.exists() {
                read_json(&id_path)?
            } else {
                let mut entropy = [0u8; 24];
                getrandom::fill(&mut entropy).map_err(|e| anyhow!("migration identity: {e}"))?;
                let id = format!("mount-migrate-{}", blake3::hash(&entropy).to_hex());
                atomic_json(&id_path, &id)?;
                id
            };
            atomic_json(&dir.join("source.json"), &old)?;
            let receipt = dir.join("verified.json");
            let manifest = if receipt.exists() {
                let saved: MigrationReceipt = read_json(&receipt)?;
                if saved.old != entry.manifest {
                    bail!("migration receipt source mismatch");
                }
                validate_component(&saved.new)?;
                let path = self.metadata.join("archives").join(&saved.new);
                let candidate: Manifest = read_json(&path)?;
                if format!(
                    "{}.json",
                    crate::manifest::manifest_fingerprint(&candidate)?
                ) != saved.new
                    || candidate.original_size != entry.size
                    || candidate.archive_id != id
                {
                    bail!("migration receipt destination mismatch");
                }
                if affected(&candidate) {
                    bail!("migration receipt targets are no longer eligible; original retained");
                }
                verify(&path)?;
                candidate
            } else {
                let content = dir.join("content");
                fs::create_dir_all(&content)?;
                require_directory(&content)?;
                validate_component(&old.original_name)?;
                let staged = content.join(&old.original_name);
                restore(&old_path, &staged)?;
                File::open(&staged)?.sync_all()?;
                if fs::metadata(&staged)?.len() != entry.size
                    || hash_file_range(&staged, 0, entry.size)? != entry.hash
                {
                    bail!("migration restored content mismatch; original retained");
                }
                upload(&staged, &id)?
            };
            crate::manifest::validate_manifest(&manifest)?;
            if manifest.original_size != entry.size
                || manifest.archive_id != id
                || affected(&manifest)
            {
                bail!("migration upload identity/placement mismatch");
            }
            let fingerprint = crate::manifest::manifest_fingerprint(&manifest)?;
            let filename = format!("{fingerprint}.json");
            let destination = self.metadata.join("archives").join(&filename);
            atomic_json(&destination, &manifest)?;
            // Durable receipt precedes active reference switch. No old shard or
            // immutable event is deleted: other PCs/history may still use them.
            atomic_json(
                &dir.join("verified.json"),
                &MigrationReceipt {
                    old: entry.manifest.clone(),
                    new: filename.clone(),
                    originals_retained: true,
                },
            )?;
            let mut next = self.catalog.clone();
            next.entries
                .get_mut(&name)
                .context("migration entry disappeared")?
                .manifest = filename;
            self.checkpoint(next)?;
            if index {
                if let Err(e) =
                    crate::inventory::add_manifest(&self.rclone, &destination.to_string_lossy())
                {
                    eprintln!("Migrated archive receipt retained; inventory update failed: {e:#}");
                }
            }
            moved += 1;
            println!("Migrated active archive {name}; original remote bytes retained for history and other PCs");
        }
        Ok(moved)
    }
}

#[derive(Serialize, Deserialize)]
struct MigrationReceipt {
    old: String,
    new: String,
    originals_retained: bool,
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fake_upload, fixture};
    use super::*;
    fn affected(m: &Manifest) -> bool {
        !m.archive_id.starts_with("mount-migrate-")
    }
    fn prepared() -> (tempfile::TempDir, Workspace) {
        let (root, mut w) = fixture();
        fs::write(w.files.join("report.txt"), b"abc").unwrap();
        w.sync_with(fake_upload, false).unwrap();
        (root, w)
    }
    #[test]
    fn migration_failure_preserves_catalog_plaintext_and_old_manifest() {
        let (_root, mut w) = prepared();
        let old = w.catalog.entries["report.txt"].manifest.clone();
        fs::write(w.files.join("report.txt"), b"pending edit").unwrap();
        let result = w.migrate_with(
            affected,
            |_, p| {
                fs::write(p, b"abc")?;
                Ok(())
            },
            |_, _| bail!("injected verification failure"),
            |_| Ok(()),
            false,
        );
        assert!(result.is_err());
        assert_eq!(w.catalog.entries["report.txt"].manifest, old);
        assert!(w.metadata.join("archives").join(old).exists());
        assert_eq!(
            fs::read(w.files.join("report.txt")).unwrap(),
            b"pending edit"
        );
    }
    #[test]
    fn migration_switches_only_active_references_and_retains_sources() {
        let (_root, mut w) = prepared();
        let old = w.catalog.entries["report.txt"].manifest.clone();
        assert_eq!(
            w.migrate_with(
                affected,
                |_, p| {
                    fs::write(p, b"abc")?;
                    Ok(())
                },
                fake_upload,
                |_| Ok(()),
                false
            )
            .unwrap(),
            1
        );
        assert_ne!(w.catalog.entries["report.txt"].manifest, old);
        assert!(w.metadata.join("archives").join(old).exists());
        assert_eq!(fs::read(w.files.join("report.txt")).unwrap(), b"abc");
        assert_eq!(
            w.migrate_with(
                affected,
                |_, _| panic!(),
                |_, _| panic!(),
                |_| panic!(),
                false
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn migration_receipt_recovers_without_restore_or_upload_but_reverifies() {
        let (_root, mut w) = prepared();
        let original = w.catalog.clone();
        w.migrate_with(
            affected,
            |_, p| {
                fs::write(p, b"abc")?;
                Ok(())
            },
            fake_upload,
            |_| Ok(()),
            false,
        )
        .unwrap();
        let migrated = w.catalog.entries["report.txt"].manifest.clone();
        // Simulate crash after receipt but before checkpoint.
        w.checkpoint(original).unwrap();
        let mut verified = 0;
        assert_eq!(
            w.migrate_with(
                affected,
                |_, _| panic!("no restore expected"),
                |_, _| panic!("no additional quota/upload expected"),
                |_| {
                    verified += 1;
                    Ok(())
                },
                false
            )
            .unwrap(),
            1
        );
        assert_eq!(verified, 1);
        assert_eq!(w.catalog.entries["report.txt"].manifest, migrated);
    }
    #[test]
    fn migration_rejects_mount_lease_and_dirty_cache() {
        let (_root, mut w) = prepared();
        fs::write(w.metadata.join("mount-process.json"), b"{}").unwrap();
        assert!(w
            .migrate_with(
                affected,
                |_, _| panic!(),
                |_, _| panic!(),
                |_| panic!(),
                false
            )
            .is_err());
        fs::remove_file(w.metadata.join("mount-process.json")).unwrap();
        let cache = w.files.parent().unwrap().join("vfs-cache");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("dirty"), b"pending").unwrap();
        assert!(w
            .migrate_with(
                affected,
                |_, _| panic!(),
                |_, _| panic!(),
                |_| panic!(),
                false
            )
            .is_err());
    }
}
