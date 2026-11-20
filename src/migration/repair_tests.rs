use super::{
    exact,
    model::{Action, Entry},
    test_support::*,
};
use crate::prelude::*;
#[test]
fn removed_root_freezes_whole_copy_destinations() {
    let source = manifest("removed", 2097152, 1048576, 1, 0, &["old:"]);
    let target = policy(&["new:"], 1048576, 1, 0);
    let mut moves = BTreeMap::new();
    let spec = exact::freeze(&source, &target, &mut moves, false, &BTreeSet::new()).unwrap();
    assert_eq!(moves.len(), source.shards.len());
    assert!(spec
        .objects
        .iter()
        .all(|o| o.destination_root == "new:" && !o.keep_ref));
}
#[test]
fn scratch_is_charged_once_per_entry() {
    let source = manifest("scratch", 2097152, 1048576, 1, 0, &["a:"]);
    let target = policy(&["a:"], 1048576, 1, 0);
    let q = crate::storage::admin::budget::TargetBudget {
        remote: "a:".into(),
        backing: "a:".into(),
        capacity_domain: "a".into(),
        failure_domain: None,
        declared: true,
        total: 1 << 40,
        free: 1 << 39,
        shard_cap: None,
    };
    let mut spec = exact::freeze(
        &source,
        &target,
        &mut BTreeMap::new(),
        false,
        &BTreeSet::new(),
    )
    .unwrap();
    spec.bindings.insert(
        q.remote.clone(),
        (q.backing.clone(), q.capacity_domain.clone(), None),
    );
    let entry = Entry {
        archive_id: source.archive_id.clone(),
        original_name: source.original_name.clone(),
        size: source.original_size,
        source: "a:source".into(),
        fingerprint: spec.source_fingerprint.clone(),
        action: Action::Relocate,
        download_bytes: 0,
        upload_bytes: 0,
        losses: vec![],
        detail: None,
        moves: BTreeMap::new(),
        exact: Some(spec.clone()),
    };
    let costs = super::storage_preview::specs(&entry, &[q], "attempt").unwrap();
    let scratch: u64 = costs.iter().map(|o| o.local_scratch_bytes.unwrap()).sum();
    assert!(
        scratch <= spec.local_scratch_bytes * 3 + 1048576,
        "scratch duplicated: {scratch}"
    );
}

fn quota(remote: &str) -> crate::storage::admin::budget::TargetBudget {
    crate::storage::admin::budget::TargetBudget {
        remote: remote.into(),
        backing: remote.into(),
        capacity_domain: remote.into(),
        failure_domain: Some(remote.into()),
        declared: true,
        total: 1 << 40,
        free: 1 << 39,
        shard_cap: None,
    }
}
fn entry(source: &Manifest, action: Action) -> Entry {
    Entry {
        archive_id: source.archive_id.clone(),
        original_name: source.original_name.clone(),
        size: source.original_size,
        source: "fake:manifest.json".into(),
        fingerprint: crate::manifest::manifest_fingerprint(source).unwrap(),
        action,
        download_bytes: 0,
        upload_bytes: 0,
        losses: Vec::new(),
        detail: None,
        moves: BTreeMap::new(),
        exact: None,
    }
}
#[test]
fn full_copy_shape_frozen_for_reencode_and_reapply_with_smaller_coded_shards() {
    let source = manifest("full-copy", 70000, 1048576, 1, 0, &["old:"]);
    let target = policy(&["a:", "b:", "c:"], 1048576, 2, 1);
    let quotas = target.remotes.iter().map(|r| quota(r)).collect::<Vec<_>>();
    for action in [Action::Reencode, Action::ReapplyPlacement] {
        let mut e = entry(&source, action);
        e.exact = Some(super::full_copy::freeze(&source, &target, &quotas).unwrap());
        exact::validate(&e, &source, &target).unwrap();
        let plan = super::full_copy::upload_plan(&e, &target, "new-id").unwrap();
        assert!(plan.shard_size < 1048576);
        assert_eq!(plan.shards.len(), e.exact.as_ref().unwrap().objects.len());
        let preview = super::storage_preview::preview(&quotas, [e.clone()]);
        assert!(preview.admitted, "{preview:?}");
        let costs = super::storage_preview::specs(&e, &quotas, "attempt")
            .unwrap()
            .iter()
            .map(super::storage_budget::calculate_output_cost)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(
            costs.iter().map(|o| o.actual_created_bytes).sum::<u64>()
                <= e.exact.as_ref().unwrap().additional_bytes
        );
        let mut runtime = target.clone();
        runtime.workers += 7;
        runtime.retries += 3;
        exact::validate(&e, &source, &runtime).unwrap();
        runtime.native_crypt = !runtime.native_crypt;
        assert!(exact::validate(&e, &source, &runtime).is_err());
    }
}
#[test]
fn legacy_exact_v1_uses_original_target_not_runtime_throttle() {
    let source = manifest("legacy-exact", 2097152, 1048576, 1, 0, &["a:"]);
    let target = policy(&["a:"], 1048576, 1, 0);
    let mut e = entry(&source, Action::Relocate);
    let mut spec = exact::freeze(&source, &target, &mut e.moves, false, &BTreeSet::new()).unwrap();
    spec.capability = "exact-immutable-execution-v1".into();
    spec.layout_digest = blake3::hash(&serde_json::to_vec(&(&target, &spec.objects)).unwrap())
        .to_hex()
        .to_string();
    e.exact = Some(spec);
    exact::validate(&e, &source, &target).unwrap();
    let before = serde_json::to_vec(&e).unwrap();
    let mut runtime = target.clone();
    runtime.workers += 3;
    // v1 verification needs its unmodified plan target; runtime target is separate.
    exact::validate(&e, &source, &target).unwrap();
    assert_eq!(before, serde_json::to_vec(&e).unwrap());
}

#[test]
fn missing_or_changed_capacity_binding_stays_blocked() {
    let source = manifest("bindings", 2097152, 1048576, 1, 0, &["a:"]);
    let target = policy(&["a:"], 1048576, 1, 0);
    let quotas = vec![quota("a:")];
    let mut e = entry(&source, Action::Reencode);
    e.exact = Some(super::full_copy::freeze(&source, &target, &quotas).unwrap());
    assert!(super::storage_preview::specs(&e, &[], "attempt").is_err());
    let mut changed = quotas;
    changed[0].capacity_domain = "foreign-domain".into();
    assert!(super::storage_preview::specs(&e, &changed, "attempt").is_err());
}

#[test]
fn resilient_freeze_uses_declared_alias_failure_domains() {
    let source = manifest(
        "alias-layout",
        2097152,
        1048576,
        2,
        1,
        &["alias-a:", "alias-b:", "c:"],
    );
    let mut target = policy(&["alias-a:", "alias-b:", "c:", "d:"], 1048576, 2, 1);
    target.placement = Placement::Resilient;
    let mut quotas = target.remotes.iter().map(|r| quota(r)).collect::<Vec<_>>();
    quotas[0].failure_domain = Some("same-provider".into());
    quotas[1].failure_domain = Some("same-provider".into());
    let mut moves = BTreeMap::new();
    exact::freeze_resilient_destinations(&source, &target, &mut moves, &quotas).unwrap();
    let domains = moves
        .values()
        .map(|r| {
            quotas
                .iter()
                .find(|q| &q.remote == r)
                .unwrap()
                .failure_domain
                .clone()
                .unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(domains.len(), 3);
    let spec = exact::freeze(&source, &target, &mut moves, false, &BTreeSet::new()).unwrap();
    assert_eq!(spec.objects.len(), 3);
}
