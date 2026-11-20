//! Full-copy contracts: pure writer shape, frozen destinations, no retirement credit.
use super::exact::{ExactExecutionSpec, ExactObject};
use crate::prelude::*;
use crate::storage::admin::budget::{BudgetSnapshot, TargetBudget};

pub(crate) fn shape(size: u64, target: &PoolDefinition, id: &str) -> Result<UploadPlan> {
    let shard = crate::models::shard_size::shard_size_for(
        size,
        target.shard_bytes()?.get(),
        target.data_shards,
        target.parity_shards,
    );
    let coding = (target.parity_shards > 0).then(|| Coding {
        algorithm: RS_ALGORITHM.into(),
        data_shards: target.data_shards,
        parity_shards: target.parity_shards,
        stripe_size: EC_STRIPE_SIZE,
    });
    crate::planning::upload_shape(
        size,
        shard,
        id.into(),
        target.remotes.clone(),
        target.placement,
        coding,
    )
}
pub(crate) fn freeze(
    source: &Manifest,
    target: &PoolDefinition,
    quotas: &[TargetBudget],
) -> Result<ExactExecutionSpec> {
    if quotas.len() != target.remotes.len()
        || target
            .remotes
            .iter()
            .any(|r| !quotas.iter().any(|q| &q.remote == r))
    {
        bail!("full-copy destination quota/binding incomplete");
    }
    let plan = shape(source.original_size, target, "frozen-output")?;
    let mut order: Vec<_> = plan.shards.iter().collect();
    order.sort_by_key(|s| (s.group, s.kind == ShardKind::Parity, s.index));
    let specs = order
        .iter()
        .map(|s| PhysicalSpec {
            group: s.group,
            size: s.size,
        })
        .collect::<Vec<_>>();
    let targets = target
        .remotes
        .iter()
        .map(|r| {
            quotas
                .iter()
                .find(|q| &q.remote == r)
                .cloned()
                .context("target binding missing")
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = BudgetSnapshot {
        targets,
        ..Default::default()
    };
    let indexes = crate::placement::assign_with_budget(
        &snapshot,
        &specs,
        target.placement,
        plan.coding.as_ref().map(|c| c.parity_shards),
    )?;
    let mut objects = order
        .into_iter()
        .zip(indexes)
        .map(|(s, i)| ExactObject {
            index: s.index,
            group: s.group,
            source: String::new(),
            destination_root: snapshot.targets[i].remote.clone(),
            size: s.size,
            keep_ref: false,
            blake3: String::new(),
        })
        .collect::<Vec<_>>();
    objects.sort_by_key(|o| o.index);
    let copied = objects
        .iter()
        .try_fold(0u64, |a, o| a.checked_add(o.size))
        .context("output size overflow")?;
    let metadata = super::exact::metadata_bound(source, target, objects.len())?;
    let extra = objects
        .iter()
        .try_fold(metadata, |a, o| {
            a.checked_add(super::exact::stored_bound(o.size).ok()?)
        })
        .context("physical output bound overflow")?;
    let scratch = source
        .original_size
        .checked_add(
            source
                .shards
                .iter()
                .try_fold(0u64, |a, s| a.checked_add(s.size))
                .context("source scratch overflow")?,
        )
        .and_then(|n| n.checked_add(copied.checked_mul(2)?))
        .context("scratch overflow")?;
    Ok(ExactExecutionSpec {
        capability: "exact-full-copy-v2".into(),
        source_fingerprint: crate::manifest::manifest_fingerprint(source)?,
        layout_digest: super::exact::layout_digest(target, &objects)?,
        bindings: quotas
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
            .collect(),
        objects,
        additional_bytes: extra,
        copied_bytes: copied,
        transfer_bytes: copied
            .checked_mul(3)
            .and_then(|n| n.checked_add(source.original_size))
            .context("transfer overflow")?,
        local_scratch_bytes: scratch,
    })
}
/// Materialize the already-frozen writer plan; put resumes this file rather than optimizing again.
pub(crate) fn upload_plan(
    entry: &super::model::Entry,
    target: &PoolDefinition,
    id: &str,
) -> Result<UploadPlan> {
    let exact = entry
        .exact
        .as_ref()
        .context("missing full-copy execution contract")?;
    let mut plan = shape(entry.size, target, id)?;
    if exact.capability != "exact-full-copy-v2" || exact.objects.len() != plan.shards.len() {
        bail!("frozen full-copy shape changed");
    }
    for (s, o) in plan.shards.iter_mut().zip(&exact.objects) {
        if s.index != o.index
            || s.group != o.group
            || s.size != o.size
            || o.keep_ref
            || !target.remotes.contains(&o.destination_root)
        {
            bail!("frozen full-copy extent changed");
        }
        s.remote = o.destination_root.clone();
        s.object = crate::utils::remote_join(&s.remote, &s.object);
    }
    Ok(plan)
}
pub(crate) fn verify_output(entry: &super::model::Entry, produced: &Manifest) -> Result<()> {
    let exact = entry.exact.as_ref().context("missing output contract")?;
    if produced.shards.len() != exact.objects.len() {
        bail!("writer output count exceeds frozen contract");
    }
    let mut actual = produced.shards.iter().collect::<Vec<_>>();
    actual.sort_by_key(|s| s.index);
    for (s, o) in actual.into_iter().zip(&exact.objects) {
        if s.index != o.index
            || s.group != o.group
            || s.size != o.size
            || s.remote != o.destination_root
            || (o.keep_ref && s.object != o.source)
        {
            bail!("writer output extent/destination differs from frozen contract");
        }
    }
    Ok(())
}
