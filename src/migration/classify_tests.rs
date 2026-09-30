use super::*;
use crate::migration::test_support::{manifest, policy, set};

const MIB: u64 = 1048576;

fn none(_: &str) -> Option<String> {
    None
}

#[test]
fn reencode_on_coding_or_shard_size_change_only() {
    let m = manifest("x", 3 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    assert_eq!(reencode_reason(&m, &policy(&["a:"], MIB, 2, 1)), None);
    let reason = reencode_reason(&m, &policy(&["a:"], MIB, 3, 1)).unwrap();
    assert!(reason.contains("RS 2+1 -> RS 3+1"), "{reason}");
    let reason = reencode_reason(&m, &policy(&["a:"], 2 * MIB, 2, 1)).unwrap();
    assert!(reason.contains("shard size"), "{reason}");
    assert!(reencode_reason(&m, &policy(&["a:"], MIB, 2, 0))
        .unwrap()
        .contains("no parity"));
    let plain = manifest("p", 3 * MIB, MIB, 1, 0, &["a:"]);
    assert!(reencode_reason(&plain, &policy(&["a:"], MIB, 2, 1)).is_some());
    // Native crypt stores the same format: not a reason to re-encode.
    let mut native = policy(&["a:"], MIB, 2, 1);
    native.native_crypt = true;
    assert_eq!(reencode_reason(&m, &native), None);
    // Empty archives cannot be coded.
    let mut empty = manifest("e", 0, MIB, 1, 0, &["a:"]);
    empty.shards[0].size = 0;
    assert_eq!(
        reencode_reason(&empty, &policy(&["a:"], 2 * MIB, 2, 1)),
        None
    );
}

#[test]
fn removed_remote_moves_its_shards_and_added_remote_moves_nothing() {
    let m = manifest("x", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let p = policy(&["a:", "b:", "d:"], MIB, 2, 1);
    let (moving, reasons) = shards_to_move(&m, &p, &set(&["a:", "b:", "d:"]), &none);
    assert_eq!(moving, BTreeSet::from([2]));
    assert!(reasons[0].contains("c:"));
    let p = policy(&["a:", "b:", "c:", "d:"], MIB, 2, 1);
    let (moving, reasons) = shards_to_move(&m, &p, &set(&["a:", "b:", "c:", "d:"]), &none);
    assert!(moving.is_empty() && reasons.is_empty());
}

#[test]
fn resilient_moves_shards_over_the_outage_bound() {
    let m = manifest("x", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let mut p = policy(&["a:", "b:", "c:"], MIB, 2, 1);
    let target = set(&["a:", "b:", "c:"]);
    let shared = |r: &str| Some(if r == "c:" { "g2" } else { "g1" }.to_owned());
    // Round robin makes no outage promise: nothing moves.
    assert!(shards_to_move(&m, &p, &target, &shared).0.is_empty());
    p.placement = Placement::Resilient;
    let (moving, reasons) = shards_to_move(&m, &p, &target, &shared);
    assert_eq!(moving, BTreeSet::from([1]), "{reasons:?}");
    let distinct = |r: &str| Some(r.to_owned());
    assert!(shards_to_move(&m, &p, &target, &distinct).0.is_empty());
    // Undeclared groups are not guessed.
    let (moving, reasons) = shards_to_move(&m, &p, &target, &none);
    assert!(moving.is_empty());
    assert!(reasons[0].contains("not declared"));
}
