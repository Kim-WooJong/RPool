//! Frozen object-level execution contract; physical additions never spend retire credit.
use super::model::Action;
use crate::prelude::*;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExactObject {
    pub index: u32,
    pub group: u32,
    pub source: String,
    pub destination_root: String,
    pub size: u64,
    pub keep_ref: bool,
    pub blake3: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExactExecutionSpec {
    pub capability: String,
    pub source_fingerprint: String,
    pub layout_digest: String,
    pub objects: Vec<ExactObject>,
    #[serde(default)]
    pub bindings: BTreeMap<String, (String, String, Option<String>)>,
    pub additional_bytes: u64,
    pub copied_bytes: u64,
    pub transfer_bytes: u64,
    pub local_scratch_bytes: u64,
}
pub(crate) fn freeze(
    source: &Manifest,
    target: &PoolDefinition,
    moves: &mut BTreeMap<u32, String>,
    minimal: bool,
    force_create: &BTreeSet<u32>,
) -> Result<ExactExecutionSpec> {
    let fp = crate::manifest::manifest_fingerprint(source)?;
    freeze_with_fingerprint(source, target, moves, minimal, force_create, fp)
}
/// Planning already fingerprinted this exact immutable manifest during load.
/// Execution uses `freeze` above and always recomputes the source fingerprint.
pub(super) fn freeze_loaded(
    loaded: &super::enumerate::Loaded,
    target: &PoolDefinition,
    moves: &mut BTreeMap<u32, String>,
    minimal: bool,
    force_create: &BTreeSet<u32>,
) -> Result<ExactExecutionSpec> {
    freeze_with_fingerprint(
        &loaded.manifest,
        target,
        moves,
        minimal,
        force_create,
        loaded.fingerprint.clone(),
    )
}
fn freeze_with_fingerprint(
    source: &Manifest,
    target: &PoolDefinition,
    moves: &mut BTreeMap<u32, String>,
    minimal: bool,
    force_create: &BTreeSet<u32>,
    fp: String,
) -> Result<ExactExecutionSpec> {
    if source.archive_id.len() > 256
        || target.remotes.iter().any(|r| r.len() > 1024)
        || source
            .shards
            .iter()
            .any(|s| s.object.len() > 4096 || s.remote.len() > 1024)
    {
        bail!("metadata address length exceeds conservative exact bound");
    }
    let remotes = &target.remotes;
    if remotes.is_empty() {
        bail!("no execution destinations");
    }
    // Freeze the same whole-output placement the relocation writer validates.
    let domains: Vec<_> = remotes
        .iter()
        .map(|r| r.split_once(':').map_or(r.as_str(), |(n, _)| n).to_owned())
        .collect();
    let slots = source
        .shards
        .iter()
        .map(|s| crate::placement::ReplacementSlot {
            group: s.group,
            size: s.size,
            current: moves
                .get(&s.index)
                .or(Some(&s.remote))
                .and_then(|r| remotes.iter().position(|x| x == r)),
        })
        .collect::<Vec<_>>();
    let (bound, strict) = relocation_bound(source, target);
    // Frozen complete maps must not be optimized a second time (resilient
    // account aliases are validated against live declared domains by writer).
    let complete = moves.len() == source.shards.len()
        && source
            .shards
            .iter()
            .all(|s| moves.get(&s.index).is_some_and(|r| remotes.contains(r)));
    let assigned = if complete {
        source
            .shards
            .iter()
            .map(|s| {
                remotes
                    .iter()
                    .position(|r| Some(r) == moves.get(&s.index))
                    .context("missing destination")
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        crate::placement::assign_replacement_slots(&domains, &slots, bound, strict)?
    };
    for (s, dst) in source.shards.iter().zip(assigned) {
        if let Some(pin) = moves.get(&s.index) {
            if pin != &remotes[dst] {
                bail!("frozen destination conflicts with placement safety");
            }
        }
        moves.insert(s.index, remotes[dst].clone());
    }
    let mut objects = Vec::new();
    let mut extra = 0u64;
    let mut copied = 0u64;
    for s in &source.shards {
        let dst = moves
            .get(&s.index)
            .cloned()
            .unwrap_or_else(|| s.remote.clone());
        if !remotes.contains(&dst) {
            bail!(
                "exact destination unavailable for shard {}; use explicit rebalance plan",
                s.index
            );
        }
        moves.insert(s.index, dst.clone());
        let keep = minimal && dst == s.remote && !force_create.contains(&s.index);
        if !keep {
            copied = copied.checked_add(s.size).context("copy byte overflow")?;
            extra = extra
                .checked_add(
                    s.size
                        .checked_add(32 + (s.size / 65536 + 1) * 16)
                        .context("crypto overhead overflow")?,
                )
                .context("peak overflow")?;
        }
        objects.push(ExactObject {
            index: s.index,
            group: s.group,
            source: s.object.clone(),
            destination_root: dst,
            size: s.size,
            keep_ref: keep,
            blake3: s.blake3.clone(),
        });
    }
    // Replicas + source fence + journal/publication metadata, conservatively per root.
    let manifest_bound = (serde_json::to_vec(source)?.len() as u64)
        .checked_mul(8)
        .and_then(|n| n.checked_add((source.shards.len() as u64).checked_mul(4096)?))
        .and_then(|n| n.checked_add(1024 * 1024))
        .context("metadata bound overflow")?;
    extra = extra
        .checked_add(
            manifest_bound
                .checked_mul(remotes.len() as u64)
                .context("metadata peak overflow")?,
        )
        .context("metadata peak overflow")?;
    let digest = layout_digest(target, &objects)?;
    Ok(ExactExecutionSpec {
        bindings: BTreeMap::new(),
        capability: "exact-immutable-execution-v2".into(),
        source_fingerprint: fp,
        layout_digest: digest,
        objects,
        additional_bytes: extra,
        copied_bytes: copied,
        transfer_bytes: copied
            .checked_mul(3)
            .and_then(|n| {
                n.checked_add(
                    source
                        .shards
                        .iter()
                        .try_fold(0u64, |a, s| a.checked_add(s.size))?,
                )
            })
            .context("transfer overflow")?,
        local_scratch_bytes: source
            .shards
            .iter()
            .try_fold(0u64, |a, s| a.checked_add(s.size))
            .context("scratch overflow")?,
    })
}
pub(crate) fn validate(
    entry: &super::model::Entry,
    source: &Manifest,
    target: &PoolDefinition,
) -> Result<()> {
    if matches!(entry.action, Action::Reencode | Action::ReapplyPlacement) {
        let spec = entry
            .exact
            .as_ref()
            .context("missing full-copy execution contract")?;
        if spec.capability != "exact-full-copy-v2"
            || spec.source_fingerprint != crate::manifest::manifest_fingerprint(source)?
            || spec.layout_digest != layout_digest(target, &spec.objects)?
        {
            bail!("full-copy source/layout changed; replan");
        }
        super::full_copy::upload_plan(entry, target, "validation-only")?;
        return Ok(());
    }
    if !matches!(entry.action, Action::RelocateMinimal | Action::Relocate) {
        return Ok(());
    }
    let expected = entry
        .exact
        .as_ref()
        .context("minimal transfer requires exact execution specification")?;
    let force_create = expected
        .objects
        .iter()
        .filter(|o| !o.keep_ref)
        .map(|o| o.index)
        .collect();
    let mut actual = freeze(
        source,
        target,
        &mut entry.moves.clone(),
        entry.action == Action::RelocateMinimal,
        &force_create,
    )?;
    actual.bindings = expected.bindings.clone();
    if expected.capability == "exact-immutable-execution-v1" {
        actual.capability = expected.capability.clone();
        actual.layout_digest = blake3::hash(&serde_json::to_vec(&(target, &actual.objects))?)
            .to_hex()
            .to_string();
    }
    if &actual != expected {
        bail!("execution specification/source/layout changed; replan");
    }
    Ok(())
}
pub(crate) fn ack(plan: &super::model::Plan) -> Result<()> {
    if plan
        .entries
        .iter()
        .any(|e| e.action == Action::RelocateMinimal)
        || plan.minimal_ack.is_some()
    {
        let a = plan
            .minimal_ack
            .as_ref()
            .context("minimal transfer requires all-PC acknowledgement")?;
        if !a.all_pcs_upgraded || !a.all_pcs_stopped {
            bail!("all PCs must be upgraded and stopped; old clients must never reconnect");
        }
    }
    Ok(())
}

pub(crate) fn layout_digest(target: &PoolDefinition, objects: &[ExactObject]) -> Result<String> {
    let mut layout = target.clone();
    layout.workers = 0;
    layout.retries = 0;
    Ok(blake3::hash(&serde_json::to_vec(&(layout, objects))?)
        .to_hex()
        .to_string())
}
pub(crate) fn stored_bound(size: u64) -> Result<u64> {
    size.checked_add(32)
        .and_then(|n| n.checked_add(size.div_ceil(65536).checked_mul(16)?))
        .context("crypto bound overflow")
}
pub(crate) fn metadata_bound(
    source: &Manifest,
    target: &PoolDefinition,
    count: usize,
) -> Result<u64> {
    (serde_json::to_vec(source)?.len() as u64)
        .checked_mul(8)
        .and_then(|n| n.checked_add((count as u64).checked_mul(8192)?))
        .and_then(|n| n.checked_add(1024 * 1024))
        .and_then(|n| n.checked_mul(target.remotes.len() as u64))
        .context("metadata bound overflow")
}
pub(crate) fn relocation_bound(source: &Manifest, target: &PoolDefinition) -> (usize, bool) {
    match &source.coding {
        Some(_)
            if matches!(
                target.placement,
                Placement::FreeRatioUnbounded | Placement::CapacityFirst
            ) =>
        {
            (usize::MAX, false)
        }
        Some(c) => (c.parity_shards, target.placement == Placement::Resilient),
        None => (usize::MAX, false),
    }
}

/// Private execution preparation for historical full-copy plans. Never publish
/// this derived document back over an immutable journal plan.
pub(crate) fn prepare(
    rclone: &str,
    entry: &super::model::Entry,
    source: &Manifest,
    target: &PoolDefinition,
) -> Result<super::model::Entry> {
    let mut prepared = entry.clone();
    if prepared.exact.is_none() {
        if prepared.action == Action::RelocateMinimal {
            bail!("legacy minimal plan lacks frozen object contract; unsupported");
        }
        if crate::manifest::manifest_fingerprint(source)? != entry.fingerprint {
            bail!("source changed before legacy execution preparation");
        }
        let q = super::storage_preview::fresh(rclone, target)?;
        prepared.exact = Some(match prepared.action {
            Action::Relocate => {
                freeze_resilient_destinations(source, target, &mut prepared.moves, &q)?;
                let mut spec =
                    freeze(source, target, &mut prepared.moves, false, &BTreeSet::new())?;
                spec.bindings = q
                    .iter()
                    .map(|q| {
                        (
                            q.remote.clone(),
                            (
                                q.backing.clone(),
                                q.capacity_domain.clone(),
                                q.failure_domain.clone(),
                            ),
                        )
                    })
                    .collect();
                spec
            }
            Action::Reencode | Action::ReapplyPlacement => {
                super::full_copy::freeze(source, target, &q)?
            }
            _ => bail!("entry is not pending writable output"),
        });
        eprintln!("[migration] prepared derived legacy full-copy contract for {}; immutable plan unchanged",entry.archive_id);
    }
    validate(&prepared, source, target)?;
    Ok(prepared)
}

/// Planning and legacy preparation use declared domains, just like writer's
/// outage_domains. A complete map is then validated, never optimized again.
pub(crate) fn freeze_resilient_destinations(
    source: &Manifest,
    target: &PoolDefinition,
    moves: &mut BTreeMap<u32, String>,
    quotas: &[crate::storage::admin::budget::TargetBudget],
) -> Result<()> {
    if target.placement != Placement::Resilient {
        return Ok(());
    }
    let domains = target
        .remotes
        .iter()
        .map(|r| {
            quotas
                .iter()
                .find(|q| &q.remote == r)
                .and_then(|q| q.failure_domain.clone())
                .context("Resilient frozen layout needs every declared failure domain")
        })
        .collect::<Result<Vec<_>>>()?;
    let slots = source
        .shards
        .iter()
        .map(|s| crate::placement::ReplacementSlot {
            group: s.group,
            size: s.size,
            current: moves
                .get(&s.index)
                .or(Some(&s.remote))
                .and_then(|r| target.remotes.iter().position(|x| x == r)),
        })
        .collect::<Vec<_>>();
    let (bound, strict) = relocation_bound(source, target);
    let assigned = crate::placement::assign_replacement_slots(&domains, &slots, bound, strict)?;
    for (s, dst) in source.shards.iter().zip(assigned) {
        if moves
            .get(&s.index)
            .is_some_and(|r| r != &target.remotes[dst])
        {
            bail!("pinned destination conflicts with declared failure-domain safety");
        }
        moves.insert(s.index, target.remotes[dst].clone());
    }
    Ok(())
}
