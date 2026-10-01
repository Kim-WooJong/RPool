//! Reads one metadata generation of a pool's drive straight from the cloud,
//! without a workspace: the files (path, revision, plaintext hash and size,
//! payload manifest) a fresh mount of that generation would show. Read-only;
//! a v7 read materializes in a throwaway workspace in a temp dir. Used by
//! pool change migration to plan and adopt the drive.
use super::shared_model::Event;
use super::virtual_drive::{drive_metadata_roots, VirtualDrive};
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
    let roots = drive_metadata_roots(
        pool,
        &policy.remotes,
        generation.epoch.as_deref(),
        generation.v7,
    )?;
    if generation.v7 {
        let records = super::peer_snapshot::migration::collect(rclone, &roots, policy.native_crypt)
            .context("drive (v7) metadata read failed")?;
        let temp = tempfile::tempdir()?;
        let scratch = VirtualDrive::open(
            rclone,
            pool,
            &temp.path().join("workspace"),
            "rpool-migration",
            None,
            1 << 20,
            false,
            true,
            true,
        )?;
        let (files, history_limit) = super::peer_snapshot::migration::view(&scratch, &records)?;
        return Ok(SourceView {
            files,
            history_limit,
        });
    }
    // Includes events kept only in checkpoints.
    let events = super::metadata_pool::read_v6(rclone, pool, policy, generation.epoch.as_deref())
        .context("drive (v6) metadata read failed")?;
    Ok(SourceView {
        files: v6_view(&events)?,
        history_limit: None,
    })
}

/// The visible files of a set of v6 events (as `Namespace::resolved`).
pub(crate) fn v6_view(events: &BTreeMap<String, Event>) -> Result<Vec<DriveFile>> {
    if events.is_empty() {
        return Ok(vec![]);
    }
    Ok(super::peer_projection::project(events)?
        .files
        .into_iter()
        .filter_map(|(path, resolved)| {
            resolved.event.content.map(|content| DriveFile {
                path,
                revision: resolved.event_id,
                hash: content.hash,
                size: content.size,
                manifest: content.manifest,
            })
        })
        .collect())
}

/// Decodes published adoption records the way a fresh PC reads them (tests
/// of the migration side, which cannot open a real workspace).
#[cfg(test)]
pub(crate) fn decode(
    records: &[super::drive_generation_write::Record],
    v7: bool,
) -> Result<SourceView> {
    if v7 {
        let mut set = super::peer_snapshot::migration::Records::default();
        for record in records {
            let kind = match record.kind {
                "snapshots" => &mut set.snapshots,
                "names" => &mut set.names,
                other => bail!("unexpected v7 record kind {other}"),
            };
            kind.insert(record.id.clone(), record.bytes.clone());
        }
        let dir = tempfile::tempdir()?;
        let mut scratch = super::virtual_drive::fixture(dir.path());
        scratch.peer_retention = true;
        scratch.state.lock().unwrap().version = 7;
        let (files, history_limit) = super::peer_snapshot::migration::view(&scratch, &set)?;
        return Ok(SourceView {
            files,
            history_limit,
        });
    }
    let mut events = BTreeMap::new();
    for record in records {
        if record.kind != "events" {
            bail!("unexpected v6 record kind {}", record.kind);
        }
        let event: Event = serde_json::from_slice(&record.bytes)?;
        event.validate()?;
        if event.id()? != record.id {
            bail!("event identity mismatch");
        }
        events.insert(record.id.clone(), event);
    }
    Ok(SourceView {
        files: v6_view(&events)?,
        history_limit: None,
    })
}
