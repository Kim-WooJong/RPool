use super::*;
use crate::migration::test_support::{manifest, set};

const MIB: u64 = 1048576;

fn listing(m: &Manifest, skip: &[u32], bad: &[u32]) -> BTreeMap<String, RemoteListing> {
    let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for s in &m.shards {
        if skip.contains(&s.index) {
            out.entry(s.remote.clone()).or_default();
            continue;
        }
        let rel = relative_remote_object(&s.remote, &s.object).unwrap();
        let size = if bad.contains(&s.index) {
            s.size + 1
        } else {
            s.size
        };
        out.entry(s.remote.clone()).or_default().insert(rel, size);
    }
    out.into_iter()
        .map(|(r, f)| (r, RemoteListing::Listed(f)))
        .collect()
}

#[test]
fn quick_states_classify_presence_size_and_remote_status() {
    let m = manifest("x", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let target = set(&["a:", "b:", "c:"]);
    let mut listings = listing(&m, &[0], &[1]);
    let states = quick_states(&m, &listings, &target);
    assert_eq!(
        states,
        vec![
            ShardState::Missing(MissingReason::Missing),
            ShardState::Missing(MissingReason::BadSize),
            ShardState::Ok
        ]
    );
    // A pool remote that cannot be listed is unknown, never missing.
    listings.insert("c:".into(), RemoteListing::Failed("timeout".into()));
    assert!(matches!(
        quick_states(&m, &listings, &target)[2],
        ShardState::Unknown(_)
    ));
    // The same remote after it left the pool counts as removed.
    let target = set(&["a:", "b:"]);
    assert_eq!(
        quick_states(&m, &listings, &target)[2],
        ShardState::Missing(MissingReason::RemoteRemoved)
    );
    listings.insert("c:".into(), RemoteListing::NotConfigured);
    assert_eq!(
        quick_states(&m, &listings, &target)[2],
        ShardState::Missing(MissingReason::RemoteRemoved)
    );
    // A removed remote that is still readable is judged by its contents.
    let listings = listing(&m, &[], &[]);
    assert_eq!(quick_states(&m, &listings, &target)[2], ShardState::Ok);
}

#[test]
fn lost_needs_fewer_than_k_even_counting_unknowns() {
    let m = manifest("x", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let miss = ShardState::Missing(MissingReason::Missing);
    let unknown = ShardState::Unknown("e".into());
    let ok = ShardState::Ok;
    let a = assess(&m, &[miss.clone(), ok.clone(), ok.clone()]);
    assert!(!a.lost && !a.undetermined);
    assert_eq!(a.losses.len(), 1);
    assert_eq!(a.losses[0].available, 2);
    assert_eq!(a.losses[0].required_k, 2);
    let a = assess(&m, &[miss.clone(), miss.clone(), ok.clone()]);
    assert!(a.lost);
    assert_eq!(a.losses[0].missing.len(), 2);
    let a = assess(&m, &[miss.clone(), unknown.clone(), ok.clone()]);
    assert!(!a.lost && a.undetermined);
    assert_eq!(a.losses[0].missing[1].reason, MissingReason::ProviderError);
    let a = assess(&m, &[miss.clone(), miss, unknown]);
    assert!(a.lost, "unknowns cannot save a group that is short anyway");
    assert_eq!(assess(&m, &[ok.clone(), ok.clone(), ok]).losses, vec![]);
}

#[test]
fn partial_final_group_counts_virtual_zero_slots() {
    // One data shard + one parity in a 2+1 group: the missing slot is zero.
    let m = manifest("x", MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    assert_eq!(m.shards.len(), 2);
    let miss = ShardState::Missing(MissingReason::Missing);
    let a = assess(&m, &[miss.clone(), ShardState::Ok]);
    assert!(!a.lost);
    assert_eq!(a.losses[0].available, 2);
    assert!(assess(&m, &[miss.clone(), miss]).lost);
}

#[test]
fn uncoded_archive_needs_every_shard() {
    let m = manifest("x", 2 * MIB, MIB, 1, 0, &["a:", "b:"]);
    let a = assess(
        &m,
        &[ShardState::Ok, ShardState::Missing(MissingReason::Missing)],
    );
    assert!(a.lost);
    assert_eq!(a.losses[0].required_k, 2);
}

#[test]
fn full_probe_maps_errors_by_pool_membership() {
    let m = manifest("x", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let probes = vec![
        (
            m.shards[0].clone(),
            Probe::Corrupt {
                found: "x".into(),
                expected: "y".into(),
            },
        ),
        (m.shards[1].clone(), Probe::Error("503".into())),
        (m.shards[2].clone(), Probe::Error("no section".into())),
    ];
    let states = full_states(&m, &probes, &set(&["a:", "b:"]));
    assert_eq!(states[0], ShardState::Missing(MissingReason::Corrupt));
    assert!(matches!(states[1], ShardState::Unknown(_)));
    assert_eq!(states[2], ShardState::Missing(MissingReason::RemoteRemoved));
}
