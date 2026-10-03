//! Synthetic save/clone benchmark: `cargo test --release namespace_save_benchmark
//! -- --ignored --nocapture`. Reports the mean time of the drive's mutation
//! pattern (`clone`, mutate, `save`) at several event counts.
use super::*;
use crate::prelude::Shard;
use std::time::{Duration, Instant};

fn hex(seed: &str) -> String {
    blake3::hash(seed.as_bytes()).to_hex().to_string()
}

pub(super) fn content(seed: &str, size: u64) -> Content {
    let shards = vec![Shard {
        index: 0,
        offset: 0,
        size,
        remote: "provider-alpha".into(),
        object: hex(&format!("object-{seed}")),
        blake3: hex(&format!("shard-{seed}")),
        kind: Default::default(),
        group: 0,
        slot: 0,
    }];
    let manifest = Manifest {
        version: 2,
        archive_id: format!("virtual-{}", hex(seed)),
        original_name: format!("synthetic-{seed}.bin"),
        original_size: size,
        shard_size: 1 << 26,
        created_unix: 0,
        content_root_blake3: crate::manifest::content_root_v2(size, 1 << 26, &None, &shards),
        coding: None,
        shards,
    };
    Content {
        hash: hex(&format!("content-{seed}")),
        size,
        manifest,
        pack: None,
    }
}

pub(super) fn event(path: &str, parents: Vec<String>, seed: &str) -> Event {
    Event {
        version: 1,
        worker: "bench".into(),
        device: "device".into(),
        path: path.into(),
        parents,
        content: Some(content(seed, 4096)),
    }
}

/// A version-6 namespace with `count` committed files in 100-file folders.
pub(super) fn synthetic(count: usize) -> Namespace {
    let mut ns = Namespace::create("bench").unwrap();
    ns.version = 6;
    for i in 0..count {
        let e = event(
            &format!("d{}/file-{i}.bin", i / 100),
            vec![],
            &i.to_string(),
        );
        ns.events.insert(e.id().unwrap(), e);
    }
    ns
}

pub(super) fn intent(path: &str, seed: &str, spool: bool) -> Intent {
    let id = hex(&format!("intent-{seed}"));
    Intent {
        id: id.clone(),
        path: path.into(),
        event_path: path.into(),
        parents: vec![],
        spool: spool.then_some(id),
        size: 4096,
        hash: hex(&format!("content-{seed}")),
        depends_on: None,
    }
}

fn mean(total: Duration, n: u32) -> f64 {
    total.as_secs_f64() * 1000.0 / f64::from(n)
}

#[test]
#[ignore]
fn namespace_save_benchmark() {
    for count in [3000usize, 20000] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = synthetic(count);
        s.save(dir.path()).unwrap();
        let bytes = fs::metadata(dir.path().join("namespace.json"))
            .unwrap()
            .len();
        let rounds = 20u32;
        let (mut clone, mut save) = (Duration::ZERO, Duration::ZERO);
        let (mut seal_save, mut commit_save, mut commit, mut validate) = (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        );
        // Seal (pending intent), then commit it: the two common mutations.
        for round in 0..rounds {
            let seed = format!("round-{round}");
            let path = format!("new/file-{round}.bin");
            let t = Instant::now();
            let mut next = s.clone();
            clone += t.elapsed();
            next.pending.push(intent(&path, &seed, true));
            let t = Instant::now();
            next.validate().unwrap();
            validate += t.elapsed();
            let t = Instant::now();
            next.save(dir.path()).unwrap();
            save += t.elapsed();
            seal_save += t.elapsed();
            s = next;

            let t = Instant::now();
            let mut next = s.clone();
            clone += t.elapsed();
            let pending = next.pending[0].clone();
            let t = Instant::now();
            next.commit(&pending, Some(content(&seed, 4096))).unwrap();
            commit += t.elapsed();
            let t = Instant::now();
            next.save(dir.path()).unwrap();
            save += t.elapsed();
            commit_save += t.elapsed();
            s = next;
        }
        let n = rounds * 2;
        eprintln!(
            "bench events={count} checkpoint={}KiB clone={:.3}ms save={:.3}ms (mean of {n})",
            bytes / 1024,
            mean(clone, n),
            mean(save, n),
        );
        eprintln!(
            "bench events={count} seal-save={:.3}ms (validate {:.3}ms) commit={:.3}ms commit-save={:.3}ms",
            mean(seal_save, rounds),
            mean(validate, rounds),
            mean(commit, rounds),
            mean(commit_save, rounds),
        );
        let probe = dir.path().join("fsync-probe");
        let mut file = File::create(&probe).unwrap();
        let t = Instant::now();
        for _ in 0..10 {
            file.write_all(b"x").unwrap();
            file.sync_all().unwrap();
        }
        eprintln!(
            "bench single append+fsync={:.3}ms",
            t.elapsed().as_secs_f64() * 100.0
        );
        let t = Instant::now();
        let loaded = Namespace::load(dir.path(), "bench").unwrap();
        eprintln!(
            "bench events={count} load={:.1}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
        assert_eq!(loaded.events.len(), count + rounds as usize);
    }
}
