use crate::erasure::reconstruct_group;
use crate::journal::{persist_restore_state, validate_restore_state};
use crate::manifest::{
    coding_group_count, data_shards, load_manifest_with_storage, manifest_fingerprint,
    validate_manifest,
};
use crate::prelude::*;
#[cfg(test)]
use crate::storage::reader::is_recoverable_loss;
use crate::storage::reader::{is_restore_unavailable, StorageReader};
use crate::storage::scheduler;
use crate::utils::{append_suffix, ensure_positive, now_unix, read_json};

#[cfg(test)]
#[path = "get_tests.rs"]
mod tests;

pub(crate) fn get(
    rclone: &str,
    manifest_src: &str,
    output: &Path,
    workers: usize,
    retries: u32,
) -> Result<()> {
    get_with_storage(
        &StorageReader::rclone(rclone),
        manifest_src,
        output,
        workers,
        retries,
    )
}

pub(crate) fn get_with_storage(
    reader: &StorageReader,
    manifest_src: &str,
    output: &Path,
    workers: usize,
    retries: u32,
) -> Result<()> {
    ensure_positive(workers, "workers")?;
    let manifest = load_manifest_with_storage(reader, manifest_src)?;
    validate_manifest(&manifest)?;

    match &manifest.coding {
        Some(coding) if coding.parity_shards > 0 => {
            get_erasure(reader, &manifest, coding, output, workers, retries)
        }
        _ => get_plain(reader, &manifest, output, workers, retries),
    }
}

pub(crate) fn get_plain(
    reader: &StorageReader,
    manifest: &Manifest,
    output: &Path,
    workers: usize,
    retries: u32,
) -> Result<()> {
    let state_path = prepare_output_and_state(manifest, output)?;
    let mut state: ResumeState = read_json(&state_path)?;

    let remaining: Vec<Shard> = data_shards(manifest)
        .into_iter()
        .filter(|s| !state.completed.contains(&s.index))
        .cloned()
        .collect();

    eprintln!(
        "[get] {} remaining data shards, {} workers",
        remaining.len(),
        workers
    );

    scheduler::run(
        remaining,
        workers,
        retries,
        |s| scheduler::remote_key(&s.remote),
        |shard| reader.download(shard, output, 1, true),
        scheduler::read_retry,
        |shard, result| {
            result?;
            state.completed.insert(shard.index);
            persist_restore_state(&state_path, &mut state)?;
            Ok(vec![])
        },
    )?;

    if state.completed.len() != data_shards(manifest).len() {
        bail!("restore finished without all data shards being marked complete");
    }

    fs::remove_file(&state_path).ok();
    println!("restored={}", output.display());
    Ok(())
}

pub(crate) fn get_erasure(
    reader: &StorageReader,
    manifest: &Manifest,
    coding: &Coding,
    output: &Path,
    workers: usize,
    retries: u32,
) -> Result<()> {
    let state_path = prepare_output_and_state(manifest, output)?;
    let mut state: ResumeState = read_json(&state_path)?;
    let data = data_shards(manifest);
    let groups = coding_group_count(data.len(), coding.data_shards);
    let remaining = data
        .iter()
        .filter(|s| !state.completed.contains(&s.index))
        .map(|s| (*s).clone())
        .collect();
    eprintln!(
        "[get] Reed-Solomon {}+{}, {} groups, {} workers (cross-group scheduling)",
        coding.data_shards, coding.parity_shards, groups, workers
    );
    scheduler::run(
        remaining,
        workers,
        retries,
        |s: &Shard| scheduler::remote_key(&s.remote),
        |shard| reader.download(shard, output, 1, true),
        scheduler::read_retry,
        |shard, result| {
            match result {
                Ok(()) => {
                    state.completed.insert(shard.index);
                    persist_restore_state(&state_path, &mut state)?;
                }
                Err(error) if is_restore_unavailable(&error) => {
                    eprintln!(
                        "[degraded] data shard {} unavailable; parity fallback: {error:#}",
                        shard.index
                    );
                }
                Err(error) => return Err(error),
            }
            Ok(vec![])
        },
    )?;
    // No direct writes remain when decoding begins. One group's parity staging
    // bounds disk/memory independently of archive length; its reads are parallel.
    for group in 0..groups {
        let missing: Vec<Shard> = data
            .iter()
            .filter(|s| s.group == group as u32 && !state.completed.contains(&s.index))
            .map(|s| (*s).clone())
            .collect();
        if missing.is_empty() {
            continue;
        }
        if missing.len() > coding.parity_shards {
            bail!(
                "group {} lost {} data shards but only {} parity shards exist",
                group,
                missing.len(),
                coding.parity_shards
            );
        }
        reconstruct_group(
            reader,
            manifest,
            coding,
            group as u32,
            &missing,
            output,
            retries,
            workers,
        )?;
        for shard in missing {
            state.completed.insert(shard.index);
        }
        persist_restore_state(&state_path, &mut state)?;
    }

    if state.completed.len() != data.len() {
        bail!(
            "restore finished with {}/{} data shards complete",
            state.completed.len(),
            data.len()
        );
    }

    fs::remove_file(&state_path).ok();
    println!("restored={}", output.display());
    Ok(())
}

pub(crate) fn prepare_output_and_state(manifest: &Manifest, output: &Path) -> Result<PathBuf> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let fingerprint = manifest_fingerprint(manifest)?;
    let state_path = append_suffix(output, ".rpool.resume.json");

    let resume = if state_path.exists() && output.exists() {
        read_json::<ResumeState>(&state_path).ok().filter(|state| {
            let len_ok = fs::metadata(output)
                .map(|m| m.len() == manifest.original_size)
                .unwrap_or(false);
            state.manifest_fingerprint == fingerprint && len_ok
        })
    } else {
        None
    };

    if let Some(mut state) = resume {
        let invalid = validate_restore_state(manifest, output, &mut state, &state_path)?;
        eprintln!(
            "[resume] {} verified completed data shards from {}{}",
            state.completed.len(),
            state_path.display(),
            if invalid > 0 {
                format!("; invalidated {invalid}")
            } else {
                String::new()
            }
        );
    } else {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(output)
            .with_context(|| format!("cannot create output: {}", output.display()))?;
        file.set_len(manifest.original_size)?;
        let mut state = ResumeState {
            version: 2,
            manifest_fingerprint: fingerprint,
            original_size: manifest.original_size,
            completed: BTreeSet::new(),
            updated_unix: now_unix(),
        };
        persist_restore_state(&state_path, &mut state)?;
    }

    Ok(state_path)
}
