//! Checkpoint, mark and (after a grace period) delete covered metadata
//! records. Pure over `Replica` directories and an explicit clock.
//! Deletion requires the gate record on every replica, a mark older than the
//! grace period, and the mark's checkpoint (head and all chunks) listed on
//! every replica and covering every marked record.
use super::metadata_cache::Cache;
use super::metadata_checkpoint::{covered_by, list_all, read_any, survey, Listing, Replica};
use super::metadata_checkpoint_model::{
    head_bytes, mark_bytes, Family, Head, Ids, Mark, Packer, FORMAT, MARK_IDS_MAX,
};
use super::metadata_dir::ObjectDir;
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    /// Run compaction from the mount maintenance loop.
    pub auto: bool,
    pub checkpoint_after_records: usize,
    pub checkpoint_after_mib: u64,
    pub grace_days: u64,
    pub interval_minutes: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            auto: true,
            checkpoint_after_records: 2_000,
            checkpoint_after_mib: 16,
            grace_days: 14,
            interval_minutes: 60,
        }
    }
}
impl Config {
    pub(crate) fn path() -> Result<PathBuf> {
        Ok(crate::config::app_config_dir()?.join("metadata-compaction.json"))
    }
    /// Defaults when the file is absent; an invalid file is an error.
    pub(crate) fn load() -> Result<Self> {
        let path = Self::path()?;
        let config: Self = if path.exists() {
            crate::utils::read_json(&path).with_context(|| format!("invalid {}", path.display()))?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.checkpoint_after_records == 0
            || self.checkpoint_after_mib == 0
            || self.grace_days == 0
            || self.interval_minutes == 0
        {
            bail!("metadata compaction thresholds, grace_days and interval_minutes must be at least 1");
        }
        Ok(())
    }
    pub(crate) fn grace_seconds(&self) -> u64 {
        self.grace_days.saturating_mul(86_400)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Report {
    pub family: String,
    pub dry_run: bool,
    /// Records listed in the record directories (gate excluded).
    pub records: usize,
    pub record_bytes: u64,
    pub checkpoints: usize,
    pub broken_checkpoints: usize,
    pub chunks: usize,
    pub marks: usize,
    pub covered: usize,
    pub uncovered: usize,
    pub uncovered_bytes: u64,
    /// Deletion of covered records is enabled (gate on every replica).
    pub deletion_enabled: bool,
    pub newest_checkpoint_unix: Option<u64>,
    /// New checkpoint head (written, or the count it would get on a dry run).
    pub checkpoint: Option<String>,
    pub checkpointed_records: usize,
    pub marked: usize,
    pub deletable: usize,
    pub deleted: usize,
    pub checkpoints_removed: usize,
    pub next_deletion_unix: Option<u64>,
    pub notes: Vec<String>,
}

pub(crate) struct Options {
    pub now: u64,
    pub dry_run: bool,
    /// Manual run: checkpoint any uncovered record, ignoring thresholds.
    pub force: bool,
    pub config: Config,
}

fn publish_all(dirs: &[&dyn ObjectDir], id: &str, bytes: &[u8]) -> Result<()> {
    for dir in dirs {
        dir.publish(id, bytes)?;
    }
    Ok(())
}
fn remove_listed(dirs: &[&dyn ObjectDir], listing: &Listing, id: &str) -> Result<bool> {
    let Some((_, holders)) = listing.get(id) else {
        return Ok(false);
    };
    for index in holders {
        dirs[*index].remove(id)?;
    }
    Ok(true)
}
/// Publishes the gate record to every replica (one-time opt-in).
pub(crate) fn enable_gate(family: &Family, replicas: &[Replica<'_>]) -> Result<()> {
    let id = family.gate_id();
    for replica in replicas {
        replica.records[0].publish(&id, &family.gate)?;
    }
    Ok(())
}

pub(crate) fn compact(
    family: &Family,
    replicas: &[Replica<'_>],
    cache: &mut Cache,
    options: &Options,
) -> Result<Report> {
    if replicas.is_empty() {
        bail!("metadata destinations missing");
    }
    let n = replicas.len();
    let gate = family.gate_id();
    let mut report = Report {
        family: family.name.into(),
        dry_run: options.dry_run,
        ..Default::default()
    };
    // 1. Records of every kind on every replica.
    let mut records: Vec<(Vec<&dyn ObjectDir>, Listing)> = Vec::new();
    for kind in 0..family.kinds.len() {
        let dirs: Vec<_> = replicas.iter().map(|r| r.records[kind]).collect();
        let mut listing = list_all(&dirs)?;
        if kind == 0 {
            report.deletion_enabled = listing
                .remove(&gate)
                .is_some_and(|(_, holders)| holders.len() == n);
        }
        report.records += listing.len();
        report.record_bytes += listing.values().map(|(size, _)| size).sum::<u64>();
        records.push((dirs, listing));
    }
    // 2. Checkpoints (chunks not in the cache are read and verified).
    let survey = survey(family, replicas, cache, &mut |_, _, _| Ok(()))?;
    report.checkpoints = survey.heads.len();
    report.broken_checkpoints = survey.broken;
    report.chunks = survey.chunks().len();
    report.newest_checkpoint_unix = survey.newest_unix();
    let mut covered = Ids::new();
    for head in survey.heads.values() {
        for (kind, ids) in covered_by(head, cache) {
            covered.entry(kind).or_default().extend(ids);
        }
    }
    let mut uncovered: Vec<(usize, String)> = Vec::new();
    for (kind, (_, listing)) in records.iter().enumerate() {
        let known = covered.get(family.kinds[kind]);
        for (id, (size, _)) in listing {
            if known.is_some_and(|k| k.contains(id)) {
                report.covered += 1;
            } else {
                uncovered.push((kind, id.clone()));
                report.uncovered_bytes += size;
            }
        }
    }
    report.uncovered = uncovered.len();
    // 3. Checkpoint the uncovered records.
    let due = report.uncovered >= options.config.checkpoint_after_records
        || report.uncovered_bytes >= options.config.checkpoint_after_mib.saturating_mul(1 << 20)
        || (options.force && report.uncovered > 0);
    let mut newest = survey.newest().map(|(id, head)| (id.clone(), head.clone()));
    if due && options.dry_run {
        report.checkpointed_records = report.uncovered;
        report
            .notes
            .push(format!("would checkpoint {} records", report.uncovered));
    } else if due {
        let chunk_dirs: Vec<_> = replicas.iter().map(|r| r.chunks).collect();
        let head_dirs: Vec<_> = replicas.iter().map(|r| r.heads).collect();
        let mut packer = Packer::new(family);
        let mut ready = Vec::new();
        let mut chunks = survey.chunks();
        let mut written = 0usize;
        let mut publish = |ready: &mut Vec<(String, Vec<u8>)>, chunks: &mut BTreeSet<String>| {
            for (id, bytes) in ready.drain(..) {
                publish_all(&chunk_dirs, &id, &bytes)?;
                let chunk = super::metadata_checkpoint_model::Chunk::parse(&id, &bytes, family)?;
                cache.chunks.insert(id.clone(), chunk.ids());
                chunks.insert(id);
            }
            anyhow::Ok(())
        };
        for (kind, id) in &uncovered {
            let (dirs, listing) = &records[*kind];
            let mut text = None;
            read_any(dirs, listing, id, |bytes| {
                text = Some(String::from_utf8(bytes.to_vec())?);
                Ok(())
            })?;
            if packer.push(family.kinds[*kind], id, text.unwrap(), &mut ready)? {
                written += 1;
            } else {
                report
                    .notes
                    .push(format!("record {} too large for a checkpoint", &id[..12]));
            }
            publish(&mut ready, &mut chunks)?;
        }
        ready.extend(packer.flush()?);
        publish(&mut ready, &mut chunks)?;
        if written > 0 {
            let head = Head {
                format: FORMAT,
                family: family.name.into(),
                created_unix: options.now,
                chunks: chunks.into_iter().collect(),
                records: (report.covered + written) as u64,
                bytes: report.record_bytes,
            };
            let (id, bytes) = head_bytes(&head)?;
            // Chunks first, then the head: a listed head never lacks a chunk.
            publish_all(&head_dirs, &id, &bytes)?;
            report.checkpoint = Some(id.clone());
            report.checkpointed_records = written;
            report.covered += written;
            report.uncovered -= written;
            report.checkpoints += 1;
            report.newest_checkpoint_unix = Some(options.now);
            newest = Some((id, head));
        }
    }
    // 4. Marks and deletion only behind the gate.
    let mark_dirs: Vec<_> = replicas.iter().map(|r| r.marks).collect();
    let mark_listing = list_all(&mark_dirs)?;
    report.marks = mark_listing.len();
    if !report.deletion_enabled {
        if report.marks > 0 || report.covered > 0 {
            report.notes.push(
                "deletion of checkpointed records is off (enable with --enable-deletion once every PC is upgraded)".into(),
            );
        }
        return Ok(report);
    }
    let mut marks = BTreeMap::new();
    for id in mark_listing.keys() {
        let mut mark = None;
        match read_any(&mark_dirs, &mark_listing, id, |bytes| {
            mark = Some(Mark::parse(id, bytes, family)?);
            Ok(())
        }) {
            Ok(()) => {
                marks.insert(id.clone(), mark.unwrap());
            }
            Err(error) => report
                .notes
                .push(format!("mark {} ignored: {error:#}", &id[..12])),
        }
    }
    // A freshly written head is listed everywhere (publish verified every replica).
    let everywhere =
        |id: &str| survey.everywhere(id, n) || report.checkpoint.as_deref() == Some(id);
    if let Some((head_id, head)) = &newest {
        if !marks.values().any(|m| &m.checkpoint == head_id) && everywhere(head_id) {
            let covered = covered_by(head, cache);
            let mut ids = Ids::new();
            let mut total = 0;
            for (kind, (_, listing)) in records.iter().enumerate() {
                let name = family.kinds[kind];
                for id in listing.keys() {
                    if total < MARK_IDS_MAX && covered.get(name).is_some_and(|c| c.contains(id)) {
                        ids.entry(name.to_string()).or_default().insert(id.clone());
                        total += 1;
                    }
                }
            }
            if total > 0 {
                report.marked = total;
                if !options.dry_run {
                    let mark = Mark {
                        format: FORMAT,
                        family: family.name.into(),
                        checkpoint: head_id.clone(),
                        marked_unix: options.now,
                        records: ids,
                    };
                    let (id, bytes) = mark_bytes(&mark)?;
                    publish_all(&mark_dirs, &id, &bytes)?;
                    report.marks += 1;
                    report.next_deletion_unix = Some(options.now + options.config.grace_seconds());
                }
            }
        }
    }
    let grace = options.config.grace_seconds();
    let head_dirs: Vec<_> = replicas.iter().map(|r| r.heads).collect();
    for (mark_id, mark) in &marks {
        let due = mark.marked_unix.saturating_add(grace);
        if options.now < due {
            report.next_deletion_unix = Some(report.next_deletion_unix.map_or(due, |d| d.min(due)));
            continue;
        }
        let Some(head) = survey.heads.get(&mark.checkpoint) else {
            if !survey.head_listing.contains_key(&mark.checkpoint) {
                // Its checkpoint was superseded and removed: the records are
                // covered (and marked) by the newer one.
                if !options.dry_run {
                    remove_listed(&mark_dirs, &mark_listing, mark_id)?;
                }
            } else {
                report.notes.push(format!(
                    "mark {} waits: its checkpoint is unreadable",
                    &mark_id[..12]
                ));
            }
            continue;
        };
        if !survey.everywhere(&mark.checkpoint, n) {
            report.notes.push(format!(
                "mark {} waits: its checkpoint is not on every replica",
                &mark_id[..12]
            ));
            continue;
        }
        let covered = covered_by(head, cache);
        let proven = mark.records.iter().all(|(kind, ids)| {
            covered
                .get(kind)
                .is_some_and(|c| ids.iter().all(|id| c.contains(id)))
        });
        if !proven {
            report.notes.push(format!(
                "mark {} ignored: it names records its checkpoint does not cover",
                &mark_id[..12]
            ));
            continue;
        }
        for (kind, ids) in &mark.records {
            let index = family.kinds.iter().position(|k| k == kind).unwrap();
            let (dirs, listing) = &records[index];
            for id in ids {
                if listing.contains_key(id) {
                    report.deletable += 1;
                    if !options.dry_run && remove_listed(dirs, listing, id)? {
                        report.deleted += 1;
                    }
                }
            }
        }
        let superseded: Vec<_> = survey
            .heads
            .iter()
            .filter(|(id, h)| {
                *id != &mark.checkpoint && h.chunks.iter().all(|c| head.chunks.contains(c))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !options.dry_run {
            for id in &superseded {
                if remove_listed(&head_dirs, &survey.head_listing, id)? {
                    report.checkpoints_removed += 1;
                }
            }
            remove_listed(&mark_dirs, &mark_listing, mark_id)?;
        }
    }
    report.covered = report.covered.saturating_sub(report.deleted);
    report.records = report.records.saturating_sub(report.deleted);
    Ok(report)
}
