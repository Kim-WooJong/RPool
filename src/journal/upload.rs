use crate::prelude::*;
use crate::storage::{reader::is_recoverable_loss, writer::StorageWriter};
use crate::utils::hash_file_range;
use crate::utils::{now_unix, read_json, save_json_atomic};

pub(crate) fn upload_plan_fingerprint(plan: &UploadPlan) -> Result<String> {
    let bytes = serde_json::to_vec(plan)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(crate) fn load_or_create_upload_journal(
    path: &Path,
    plan: &UploadPlan,
) -> Result<UploadJournal> {
    let fingerprint = upload_plan_fingerprint(plan)?;
    if path.exists() {
        let mut journal: UploadJournal = read_json(path)?;
        if journal.plan_fingerprint != fingerprint {
            bail!(
                "upload journal does not match the active upload plan: {}",
                path.display()
            );
        }
        journal.version = 1;
        return Ok(journal);
    }
    let journal = UploadJournal {
        version: 1,
        plan_fingerprint: fingerprint,
        completed: BTreeMap::new(),
        updated_unix: now_unix(),
    };
    save_json_atomic(path, &journal)?;
    Ok(journal)
}

pub(crate) fn validate_upload_journal(
    storage: &StorageWriter,
    source: &Path,
    plan: &UploadPlan,
    journal: &mut UploadJournal,
) -> Result<usize> {
    let mut removed = Vec::new();
    for (index, shard) in &journal.completed {
        let Some(plan_shard) = plan
            .shards
            .iter()
            .find(|candidate| candidate.index == *index)
        else {
            removed.push(*index);
            continue;
        };
        if *index != shard.index
            || shard.index != plan_shard.index
            || shard.object != plan_shard.object
            || shard.size != plan_shard.size
            || shard.kind != plan_shard.kind
            || shard.remote != plan_shard.remote
            || shard.offset != plan_shard.offset
            || shard.group != plan_shard.group
            || shard.slot != plan_shard.slot
        {
            removed.push(*index);
            continue;
        }
        // Parity is regenerated from the current immutable upload snapshot.
        if shard.kind == ShardKind::Parity
            || hash_file_range(source, shard.offset, shard.size)? != shard.blake3
        {
            removed.push(*index);
            continue;
        }
        storage.ensure_destination(&shard.object)?;
        match storage.reader().verify(shard, true) {
            Ok(()) => {}
            Err(error) if is_recoverable_loss(&error) => removed.push(*index),
            Err(error) => return Err(error),
        }
    }
    for index in &removed {
        journal.completed.remove(index);
    }
    Ok(removed.len())
}

pub(crate) fn record_upload_shard(
    path: &Path,
    journal: &Arc<Mutex<UploadJournal>>,
    shard: Shard,
) -> Result<()> {
    let mut state = journal.lock().expect("upload journal poisoned");
    state.completed.insert(shard.index, shard);
    state.updated_unix = now_unix();
    save_json_atomic(path, &*state)
}
