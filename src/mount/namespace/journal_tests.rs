//! Journal replay, torn tails, compaction, crash points, backward
//! compatibility and the incremental validation paths.
use super::bench_tests::{content, event, intent, synthetic};
use super::*;
use crate::mount::crash;

/// The persisted fields (the loading worker name aside).
fn persisted(ns: &Namespace) -> serde_json::Value {
    let mut value = serde_json::to_value(ns).unwrap();
    value["worker"] = serde_json::Value::Null;
    value
}

fn reloaded(dir: &Path) -> serde_json::Value {
    persisted(&Namespace::load(dir, "bench").unwrap())
}

fn journal_len(dir: &Path) -> u64 {
    fs::metadata(dir.join(journal::JOURNAL)).map_or(0, |m| m.len())
}

/// A saved 50-file namespace whose checkpoint is far larger than a record.
fn saved(dir: &Path) -> Namespace {
    let mut s = synthetic(50);
    s.save(dir).unwrap();
    s
}

/// One mutation of each journaled kind, cycling with `round`.
fn mutate(s: &Namespace, round: usize) -> Namespace {
    let mut next = s.clone();
    let seed = format!("m{round}");
    let path = format!("new/f{round}.bin");
    match round % 6 {
        0 => next.pending.push(intent(&path, &seed, true)),
        1 => {
            let pending = next.pending.last().cloned().unwrap();
            let id = next.commit(&pending, Some(content(&seed, 4096))).unwrap();
            next.published.insert(id);
        }
        2 => {
            next.directories.insert(format!("dir{round}"));
        }
        3 => {
            next.base(&format!("d0/file-{round}.bin")).unwrap();
        }
        4 => {
            let first = next.directories.iter().next().cloned().unwrap();
            next.directories.remove(&first);
            next.pending
                .push(intent(&format!("{path}.x"), &seed, false));
        }
        _ => {
            next.pending.clear();
        }
    }
    next
}

#[test]
fn every_save_replays_from_checkpoint_plus_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = saved(dir.path());
    let checkpoint = fs::read(dir.path().join("namespace.json")).unwrap();
    for round in 0..24 {
        let mut next = mutate(&s, round);
        next.save(dir.path()).unwrap();
        s = next;
        assert_eq!(reloaded(dir.path()), persisted(&s), "round {round}");
    }
    // All of it went to the journal: the checkpoint was never rewritten.
    assert_eq!(
        fs::read(dir.path().join("namespace.json")).unwrap(),
        checkpoint
    );
    assert!(journal_len(dir.path()) > journal::HEADER_LEN);
    // A reloaded namespace keeps appending to the same journal.
    let mut loaded = Namespace::load(dir.path(), "bench").unwrap();
    let before = journal_len(dir.path());
    loaded.directories.insert("after-reload".into());
    loaded.save(dir.path()).unwrap();
    assert!(journal_len(dir.path()) > before);
    assert_eq!(reloaded(dir.path()), persisted(&loaded));
}

#[test]
fn compaction_folds_the_journal_and_keeps_a_verified_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = saved(dir.path());
    for round in 0..4 {
        let mut next = mutate(&s, round);
        next.save(dir.path()).unwrap();
        s = next;
    }
    let expected_previous = persisted(&s);
    // A save without changes advances the previous generation (spool
    // cleanup relies on this): it always compacts.
    s.save(dir.path()).unwrap();
    assert!(!dir.path().join(journal::JOURNAL).exists());
    assert!(dir.path().join(journal::PREVIOUS_JOURNAL).exists());
    assert_eq!(reloaded(dir.path()), persisted(&s));
    let previous = Namespace::load_generation(
        dir.path(),
        "namespace.previous.json",
        journal::PREVIOUS_JOURNAL,
    )
    .unwrap();
    assert_eq!(persisted(&previous), expected_previous);
    // The journal-size threshold compacts too.
    let mut big = s.clone();
    for i in 0..400 {
        big.directories
            .insert(format!("bulk/{i:04}-{}", "x".repeat(200)));
    }
    big.save(dir.path()).unwrap();
    assert!(!dir.path().join(journal::JOURNAL).exists());
    assert_eq!(reloaded(dir.path()), persisted(&big));
}

#[test]
fn a_torn_record_is_dropped_at_load_and_overwritten_by_the_next_save() {
    for compacted_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = saved(dir.path());
        if !compacted_first {
            let mut next = mutate(&s, 0);
            next.save(dir.path()).unwrap();
            s = next;
        }
        // compacted_first: no journal yet, so the torn record is the first
        // one of a new journal.
        let acknowledged = persisted(&s);
        let mut next = mutate(&s, 2);
        crash::arm("namespace.journal_torn");
        assert!(next.save(dir.path()).is_err());
        crash::disarm();
        assert_eq!(reloaded(dir.path()), acknowledged, "{compacted_first}");
        // The live (unacknowledged-attempt) owner saves again on top.
        let mut retry = mutate(&s, 3);
        retry.save(dir.path()).unwrap();
        assert_eq!(reloaded(dir.path()), persisted(&retry));
        // A reload after the torn tail also appends cleanly.
        let mut loaded = Namespace::load(dir.path(), "bench").unwrap();
        loaded.directories.insert("x".into());
        loaded.save(dir.path()).unwrap();
        assert_eq!(reloaded(dir.path()), persisted(&loaded));
    }
}

#[test]
fn zero_fill_and_short_tails_are_torn_but_damage_before_the_tail_is_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = saved(dir.path());
    let mut states = vec![];
    for round in [0, 2, 3] {
        let mut next = mutate(&s, round);
        next.save(dir.path()).unwrap();
        s = next;
        states.push((journal_len(dir.path()), persisted(&s)));
    }
    let path = dir.path().join(journal::JOURNAL);
    let good = fs::read(&path).unwrap();
    for tail in [
        vec![0u8; 3],
        vec![0u8; 100],
        vec![7u8; 2],
        vec![9, 0, 0, 0, 1, 2],
    ] {
        let mut bytes = good.clone();
        bytes.extend_from_slice(&tail);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(reloaded(dir.path()), states[2].1, "{tail:?}");
    }
    // A damaged last record is indistinguishable from a torn append.
    let mut bytes = good.clone();
    let last = bytes.len() - 40;
    bytes[last] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert_eq!(reloaded(dir.path()), states[1].1);
    // Damage with valid-looking data after it is corruption: refuse.
    let mut bytes = good.clone();
    bytes[journal::HEADER_LEN as usize + 6] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(Namespace::load(dir.path(), "bench").is_err());
    // So is a damaged header.
    let mut bytes = good.clone();
    bytes[20] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(Namespace::load(dir.path(), "bench").is_err());
    fs::write(&path, &good).unwrap();
    assert_eq!(reloaded(dir.path()), states[2].1);
}

#[test]
fn a_stale_journal_of_a_replaced_checkpoint_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = saved(dir.path());
    let mut next = mutate(&s, 0);
    next.save(dir.path()).unwrap();
    s = next;
    let stale = fs::read(dir.path().join(journal::JOURNAL)).unwrap();
    s.save(dir.path()).unwrap(); // compaction
                                 // As if the compaction died before removing the journal.
    fs::write(dir.path().join(journal::JOURNAL), &stale).unwrap();
    assert_eq!(reloaded(dir.path()), persisted(&s));
    let mut next = mutate(&s, 2);
    next.save(dir.path()).unwrap();
    assert_eq!(reloaded(dir.path()), persisted(&next));
}

#[test]
fn every_namespace_crash_point_leaves_the_old_or_the_new_state() {
    const POINTS: &[&str] = &[
        "namespace.after_previous",
        "durable.before_persist",
        "namespace.journal_torn",
        "namespace.after_journal_append",
    ];
    let mut fired = BTreeSet::new();
    for &point in POINTS {
        // false: the save appends; true: it compacts (no-op save).
        for compaction in [false, true] {
            for fresh_journal in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let mut s = saved(dir.path());
                if !fresh_journal {
                    let mut next = mutate(&s, 0);
                    next.save(dir.path()).unwrap();
                    s = next;
                }
                let old = persisted(&s);
                let mut next = if compaction { s.clone() } else { mutate(&s, 2) };
                crash::arm(point);
                let result = next.save(dir.path());
                if crash::disarm().is_none() {
                    fired.insert(point);
                    assert!(result.is_err());
                    let after = reloaded(dir.path());
                    assert!(after == old || after == persisted(&next), "{point}");
                } else {
                    result.unwrap();
                }
                // The surviving in-memory state keeps saving correctly.
                let mut retry = mutate(&s, 3);
                retry.save(dir.path()).unwrap();
                assert_eq!(reloaded(dir.path()), persisted(&retry), "{point}");
            }
        }
    }
    assert_eq!(fired.len(), POINTS.len(), "{fired:?}");
}

#[test]
fn concurrent_copies_saving_from_one_base_end_with_the_last_save() {
    let dir = tempfile::tempdir().unwrap();
    let s = saved(dir.path());
    let mut a = mutate(&s, 0);
    let mut b = mutate(&s, 2);
    a.save(dir.path()).unwrap();
    b.save(dir.path()).unwrap();
    assert_eq!(reloaded(dir.path()), persisted(&b));
    // A copy whose checkpoint was compacted away writes a full checkpoint.
    let mut c = mutate(&b, 3);
    b.save(dir.path()).unwrap(); // compaction
    c.save(dir.path()).unwrap();
    assert_eq!(reloaded(dir.path()), persisted(&c));
    // A different directory never appends to a journal it did not write.
    let other = tempfile::tempdir().unwrap();
    let mut d = mutate(&c, 4);
    d.save(other.path()).unwrap();
    assert_eq!(reloaded(other.path()), persisted(&d));
    // A generation that skipped ahead cannot be replayed: checkpoint.
    let mut e = mutate(&d, 5);
    e.generation += 10;
    e.save(other.path()).unwrap();
    assert_eq!(reloaded(other.path()), persisted(&e));
    let mut f = mutate(&e, 2);
    f.save(other.path()).unwrap();
    assert!(other.path().join(journal::JOURNAL).exists());
    assert_eq!(reloaded(other.path()), persisted(&f));
}

/// The envelope every earlier version wrote.
fn write_v1(dir: &Path, ns: &Namespace) {
    let payload = serde_json::to_vec(ns).unwrap();
    let hash = blake3::hash(&payload).to_hex().to_string();
    let mut bytes = format!("{{\"hash\":\"{hash}\",\"payload\":").into_bytes();
    bytes.extend_from_slice(&payload);
    bytes.push(b'}');
    fs::write(dir.join("namespace.json"), bytes).unwrap();
}

#[test]
fn old_format_checkpoints_load_and_upgrade_on_the_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = synthetic(20);
    old.pending.push(intent("draft.bin", "draft", true));
    old.directories.insert("folder".into());
    write_v1(dir.path(), &old);
    let mut loaded = Namespace::load(dir.path(), "bench").unwrap();
    assert_eq!(persisted(&loaded), persisted(&old));
    // An old checkpoint is never journaled onto: the first save rewrites it
    // as the journaled envelope, and the old file becomes the previous one.
    loaded.directories.insert("new".into());
    loaded.save(dir.path()).unwrap();
    let primary = fs::read(dir.path().join("namespace.json")).unwrap();
    assert!(primary.starts_with(ENVELOPE_V2));
    assert!(fs::read(dir.path().join("namespace.previous.json"))
        .unwrap()
        .starts_with(ENVELOPE_V1));
    assert_eq!(reloaded(dir.path()), persisted(&loaded));
    // A corrupt primary still refuses to fall back to the previous one.
    fs::write(dir.path().join("namespace.json"), b"broken").unwrap();
    let error = Namespace::load(dir.path(), "bench")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("validated previous generation exists"),
        "{error}"
    );

    // A non-canonical (pretty) old envelope still verifies.
    let pretty = tempfile::tempdir().unwrap();
    let payload = serde_json::to_vec(&old).unwrap();
    let envelope = serde_json::json!({
        "hash": blake3::hash(&payload).to_hex().to_string(),
        "payload": serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
    });
    fs::write(
        pretty.path().join("namespace.json"),
        serde_json::to_vec_pretty(&envelope).unwrap(),
    )
    .unwrap();
    assert_eq!(reloaded(pretty.path()), persisted(&old));
    // A wrong checksum is refused in either layout.
    let mut tampered = fs::read(dir.path().join("namespace.previous.json")).unwrap();
    let at = tampered.len() - 10;
    tampered[at] ^= 1;
    assert!(open_envelope(&tampered, true).is_err());
}

#[test]
fn incremental_path_checks_agree_with_the_full_check() {
    let committed = ["a.txt", "Dir/x", "dir2/sub/y", "z"];
    let directories = ["Empty", "Dir/inner"];
    let candidates = [
        "a.txt",
        "A.TXT",
        "b",
        "Dir/new",
        "dir/new",
        "a.txt/x",
        "z/q",
        "dir2",
        "DIR2/sub",
        "dir2/sub/y/w",
        "empty",
        "Empty/f",
        "EMPTY/f",
        "Dir/inner",
        "new/one",
        "New/two",
    ];
    let mut base = Namespace::create("w").unwrap();
    for path in committed {
        let e = event(path, vec![], path);
        base.events.insert(e.id().unwrap(), e);
    }
    base.directories
        .extend(directories.iter().map(|d| d.to_string()));
    base.validate().unwrap();
    let mut cases = 0;
    for (i, first) in candidates.iter().enumerate() {
        for second in &candidates[i..] {
            for delete_first in [false, true] {
                let mut ns = base.clone();
                ns.pending
                    .push(intent(first, &format!("1{first}"), !delete_first));
                if second != first {
                    ns.pending.push(intent(second, &format!("2{second}"), true));
                }
                let resolved = ns.projected().unwrap();
                let mut live: BTreeSet<&str> = validation::committed_files(&resolved).collect();
                for i in &ns.pending {
                    if i.spool.is_some() {
                        live.insert(&i.path);
                    } else {
                        live.remove(i.path.as_str());
                    }
                }
                let full = validation::check_paths(&live, &ns.directories).is_ok();
                assert_eq!(
                    ns.validate().is_ok(),
                    full,
                    "{first} {second} {delete_first}"
                );
                cases += 1;
            }
        }
    }
    assert!(cases > 200);
}

#[test]
fn cached_reference_checks_still_reject_new_dangling_references() {
    let dir = tempfile::tempdir().unwrap();
    let s = saved(dir.path());
    let mut bad = s.clone();
    bad.published.insert("0".repeat(64));
    assert!(bad.save(dir.path()).is_err());
    let mut bad = s.clone();
    bad.bases.insert("p".into(), vec!["1".repeat(64)]);
    assert!(bad.validate().is_err());
    let mut bad = s.clone();
    bad.committed_intents.insert("2".repeat(64), "3".repeat(64));
    assert!(bad.validate().is_err());
    let mut bad = s.clone();
    bad.events.clear();
    bad.published
        .insert(s.events.keys().next().unwrap().clone());
    assert!(bad.validate().is_err());
    s.validate().unwrap();
}
