//! Reads one metadata generation of a pool's drive straight from the cloud,
//! without a workspace: the files (path, revision, plaintext hash and size,
//! payload manifest) a fresh mount of that generation would show. Read-only.
//! Used by pool change migration to plan and adopt the drive.
use super::shared_model::Event;
use crate::migration::drive_model::{DriveFile, GenerationRef, SourceView};
use crate::prelude::*;

/// The drive of `generation`, read from the replicas on `policy`'s remotes.
/// Any replica outage is an error (never a partial view), like a mount's pull.
pub(crate) fn read(
    rclone: &str,
    pool: &str,
    policy: &PoolDefinition,
    generation: &GenerationRef,
) -> Result<SourceView> {
    // Includes events kept only in checkpoints.
    let events = super::metadata_pool::read_v6(rclone, pool, policy, generation.epoch.as_deref())
        .context("drive (v6) metadata read failed")?;
    Ok(SourceView {
        files: v6_view(&events)?,
    })
}

/// The visible files of a set of v6 events (as `Namespace::resolved`).
pub(crate) fn v6_view(events: &BTreeMap<String, Event>) -> Result<Vec<DriveFile>> {
    if events.is_empty() {
        return Ok(vec![]);
    }
    let mut files = Vec::new();
    for (path, resolved) in super::peer_projection::project(events)?.files {
        let Some(content) = resolved.event.content else {
            continue;
        };
        // Migration re-encodes archives per file; a pack is shared by many
        // files and would be re-encoded once per member. Refuse until packs
        // migrate as one unit.
        if content.pack.is_some() {
            bail!(
                "{path} is stored in a small-file pack; pool migration does not move packed files yet"
            );
        }
        files.push(DriveFile {
            path,
            revision: resolved.event_id,
            hash: content.hash,
            size: content.size,
            manifest: content.manifest,
        });
    }
    Ok(files)
}

/// Decodes published adoption records the way a fresh PC reads them (tests
/// of the migration side, which cannot open a real workspace).
#[cfg(test)]
pub(crate) fn decode(records: &[super::drive_generation_write::Record]) -> Result<SourceView> {
    let mut events = BTreeMap::new();
    for record in records {
        let event: Event = serde_json::from_slice(&record.bytes)?;
        event.validate()?;
        if event.id()? != record.id {
            bail!("event identity mismatch");
        }
        events.insert(record.id.clone(), event);
    }
    Ok(SourceView {
        files: v6_view(&events)?,
    })
}
