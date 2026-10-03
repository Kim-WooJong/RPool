//! Small-file packs: many small sealed writes uploaded as ONE archive.
//!
//! Every archive costs K+M shard uploads, a manifest and capacity checks,
//! whatever its size; for small files that per-archive cost dominates (and
//! provider request limits bite). With `small_file_packing` on for the pool,
//! an upload round batches ready writes of at most `PACK_MEMBER_MAX` bytes
//! (`upload_round::claim`) into a pack of up to
//! `PACK_TARGET_BYTES` / `PACK_MAX_MEMBERS`. The pack is an ordinary
//! archive (`virtual-pack-<id>`), so scrub, repair, verify, cleanup and
//! manifest replicas treat it like any other; each member commits its own
//! event whose `Content` names the pack manifest plus its byte offset
//! (`shared_model::PackSlice`, event version 3).
//!
//! Safety: the staged pack is built from the sealed spool images, every
//! member's slice is re-hashed against its intent hash before upload, and a
//! member commits only after the whole pack uploaded. A failed upload leaves
//! every member pending with its spool image; the staging folder
//! (`<workspace>/packs/<id>`) is reused when the same members retry and
//! removed once they committed.
use super::*;
use crate::mount::shared_model::PackSlice;
use std::time::Duration;

/// Largest file that joins a pack.
pub(crate) const PACK_MEMBER_MAX: u64 = 1024 * 1024;
/// A pack stops growing at this many bytes.
pub(crate) const PACK_TARGET_BYTES: u64 = 16 * 1024 * 1024;
/// A pack stops growing at this many files (each member event carries the
/// pack manifest, so this also bounds metadata).
pub(crate) const PACK_MAX_MEMBERS: usize = 500;
/// How long a claim waits for more small files to join a pack.
pub(crate) const BATCH_WAIT: Duration = Duration::from_millis(1000);

/// Uploads one staged pack file as archive `id` and returns its manifest
/// (injectable for tests; the drive uses `upload_eligible_tracked`).
pub(crate) type PackUploadFn<'a> = dyn Fn(&Path, &str) -> Result<Manifest> + Sync + 'a;

impl VirtualDrive {
    /// Whether `intent` may join a pack: packing is on for this pool and it
    /// is a write of 1..=[`PACK_MEMBER_MAX`] bytes.
    pub(super) fn packable(&self, intent: &Intent) -> bool {
        self.policy.small_file_packing
            && intent.spool.is_some()
            && intent.size > 0
            && intent.size <= PACK_MEMBER_MAX
    }

    /// The drive's pack uploader: the eligible-target upload every file uses.
    pub(super) fn upload_pack_archive(&self, staged: &Path, id: &str) -> Result<Manifest> {
        let (manifest, _) = crate::mount::upload::upload_eligible_tracked(
            &self.rclone,
            &self.policy,
            &self.pool,
            staged,
            id,
        )?;
        Ok(manifest)
    }

    /// Uploads `members` (in pending order) as one pack and commits each one.
    /// Members whose bytes the drive already stores reuse that content; a
    /// lone remaining member uploads on its own (`single`). Returns one
    /// result per member, in order.
    pub(super) fn upload_pack(
        &self,
        members: &[Intent],
        upload: &PackUploadFn<'_>,
        single: &dyn Fn(&Intent) -> Result<Option<Content>>,
    ) -> Vec<Result<()>> {
        let mut results: Vec<Option<Result<()>>> = members.iter().map(|_| None).collect();
        let mut packed = Vec::new();
        for (index, member) in members.iter().enumerate() {
            match self.same_content(member) {
                Ok(Some(existing)) => {
                    results[index] = Some(self.commit_uploaded(member, Some(existing)));
                }
                Ok(None) => packed.push(index),
                Err(error) => results[index] = Some(Err(error)),
            }
        }
        match packed.as_slice() {
            [] => {}
            [only] => {
                let member = &members[*only];
                results[*only] =
                    Some(single(member).and_then(|content| self.commit_uploaded(member, content)));
            }
            _ => {
                let group: Vec<&Intent> = packed.iter().map(|&i| &members[i]).collect();
                match self.build_and_upload(&group, upload) {
                    Ok((manifest, offsets, dir)) => {
                        let mut all_committed = true;
                        for (&index, offset) in packed.iter().zip(offsets) {
                            let member = &members[index];
                            let content = Content {
                                hash: member.hash.clone(),
                                size: member.size,
                                manifest: manifest.clone(),
                                pack: Some(PackSlice { offset }),
                            };
                            let committed = self.commit_uploaded(member, Some(content));
                            all_committed &= committed.is_ok();
                            results[index] = Some(committed);
                        }
                        if all_committed {
                            let _ = fs::remove_dir_all(dir);
                        }
                    }
                    Err(error) => {
                        let text = format!("{error:#}");
                        for &index in &packed {
                            results[index] = Some(Err(anyhow!("small-file pack upload: {text}")));
                        }
                    }
                }
            }
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(anyhow!("pack member not processed"))))
            .collect()
    }

    /// Builds the staged pack of `members` (reusing a complete one of the
    /// same members), checks every member's bytes, uploads it and returns
    /// (manifest, member offsets, staging folder).
    fn build_and_upload(
        &self,
        members: &[&Intent],
        upload: &PackUploadFn<'_>,
    ) -> Result<(Manifest, Vec<u64>, PathBuf)> {
        // Same members, same pack: a retry resumes the staged upload.
        let mut key = blake3::Hasher::new();
        for member in members {
            key.update(member.id.as_bytes());
            key.update(b"\0");
        }
        let id = key.finalize().to_hex()[..32].to_string();
        let dir = self.root.join("packs").join(&id);
        fs::create_dir_all(&dir)?;
        let staged = dir.join(format!("rpool-pack-{id}.bin"));
        let mut offsets = Vec::with_capacity(members.len());
        let mut total = 0u64;
        for member in members {
            offsets.push(total);
            total = total
                .checked_add(member.size)
                .context("pack size overflow")?;
        }
        if fs::metadata(&staged).map(|m| m.len()).ok() != Some(total) {
            let partial = dir.join("building");
            let mut out = File::create(&partial)?;
            for member in members {
                let mut input = File::open(self.spool_path(member))?;
                let copied = std::io::copy(&mut (&mut input).take(member.size + 1), &mut out)?;
                if copied != member.size {
                    bail!("pack member {} changed size", member.path);
                }
            }
            out.sync_all()?;
            drop(out);
            fs::rename(&partial, &staged)?;
        }
        // The slice every member's event will name must hold its exact bytes.
        for (member, offset) in members.iter().zip(&offsets) {
            if crate::utils::hash_file_range(&staged, *offset, member.size)? != member.hash {
                let _ = fs::remove_file(&staged);
                bail!(
                    "pack member {} does not match its sealed image",
                    member.path
                );
            }
        }
        let manifest = upload(&staged, &format!("virtual-pack-{id}"))?;
        if manifest.original_size != total {
            bail!("uploaded pack size differs from the staged pack");
        }
        Ok((manifest, offsets, dir))
    }
}
