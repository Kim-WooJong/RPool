use super::*;
use crate::storage::{
    error::StorageError,
    memory::{
        faults::{Fault, FaultBackend, Gate, Operation, Rule},
        MemoryBackend,
    },
    reader::StorageReader,
    reference::{BackendId, ObjectKey, ObjectRef},
    registry::BackendRegistry,
    traits::*,
    writer::StorageWriter,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration, time::Instant};

const MANIFEST: &[u8] = include_bytes!("fixtures/v2-plain.json");
const REMOTES: [&str; 3] = ["c:", "a:", "b:"];

fn target(remote: &str) -> String {
    remote_join(remote, "golden/manifest.json")
}
fn key(remote: &str) -> ObjectKey {
    ObjectKey::new(format!("replica-{}", remote.trim_end_matches(':'))).unwrap()
}
fn remotes() -> Vec<String> {
    REMOTES.iter().map(|r| r.to_string()).collect()
}

struct Fixture {
    good: Arc<MemoryBackend>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            good: Arc::new(MemoryBackend::new(BackendId::new("good").unwrap())),
        }
    }
    /// Each `failing` remote routes to its own backend whose every write fails
    /// with an error naming that remote; `good_rules` apply to the backend
    /// shared by all other remotes.
    fn writer(&self, good_rules: Vec<Rule>, failing: &[&str]) -> StorageWriter {
        let mut registry = BackendRegistry::new();
        registry
            .register(Arc::new(
                FaultBackend::new(self.good.clone(), good_rules).unwrap(),
            ))
            .unwrap();
        let mut bindings = BTreeMap::new();
        for remote in REMOTES {
            let backend = if failing.contains(&remote) {
                let name = format!("broken-{}", remote.trim_end_matches(':'));
                let memory = Arc::new(MemoryBackend::new(BackendId::new(name).unwrap()));
                let rules = (1..=8)
                    .map(|call| Rule {
                        operation: Operation::Write,
                        call,
                        fault: Fault::Error(StorageError::Authentication {
                            detail: format!("synthetic replica failure on {remote}"),
                        }),
                    })
                    .collect();
                let id = memory.id();
                registry
                    .register(Arc::new(FaultBackend::new(memory, rules).unwrap()))
                    .unwrap();
                id
            } else {
                self.good.id()
            };
            bindings.insert(target(remote), ObjectRef::new(backend, key(remote)));
        }
        StorageWriter::synthetic(StorageReader::from_registry(
            registry,
            bindings,
            OperationContext::none(),
        ))
    }
    fn written(&self, remote: &str) -> Option<Vec<u8>> {
        self.good
            .read_all(&OperationContext::none(), &key(remote), None)
            .ok()
    }
}

#[test]
fn replicas_are_all_written_in_remote_list_order() {
    let f = Fixture::new();
    let writer = f.writer(vec![], &[]);
    let written = replicate_manifest_bytes_with_storage(&writer, MANIFEST, &remotes(), 1).unwrap();
    assert_eq!(
        written,
        REMOTES.iter().map(|r| target(r)).collect::<Vec<_>>()
    );
    for remote in REMOTES {
        assert_eq!(f.written(remote).as_deref(), Some(MANIFEST), "{remote}");
    }
}

#[test]
fn failing_remote_is_reported_after_other_replicas_finish() {
    let f = Fixture::new();
    // The middle remote fails; the sequential loop never wrote the last one.
    let writer = f.writer(vec![], &["a:"]);
    let error =
        replicate_manifest_bytes_with_storage(&writer, MANIFEST, &remotes(), 1).unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("synthetic replica failure on a:"), "{text}");
    assert!(f.written("c:").is_some());
    assert!(f.written("b:").is_some());
    assert!(f.written("a:").is_none());
}

#[test]
fn first_failing_remote_in_list_order_wins() {
    let f = Fixture::new();
    // "a:" precedes "b:" in the list, whichever thread fails first.
    let writer = f.writer(vec![], &["b:", "a:"]);
    let text = format!(
        "{:#}",
        replicate_manifest_bytes_with_storage(&writer, MANIFEST, &remotes(), 1).unwrap_err()
    );
    assert!(text.contains("synthetic replica failure on a:"), "{text}");
    assert!(!text.contains("on b:"), "{text}");
    assert!(f.written("c:").is_some());
    // Reversing the list order reverses which failure is reported.
    let reversed: Vec<String> = REMOTES.iter().rev().map(|r| r.to_string()).collect();
    let text = format!(
        "{:#}",
        replicate_manifest_bytes_with_storage(&writer, MANIFEST, &reversed, 1).unwrap_err()
    );
    assert!(text.contains("synthetic replica failure on b:"), "{text}");
}

#[test]
fn a_stalled_replica_does_not_block_the_others() {
    let f = Fixture::new();
    let gate = Gate::new();
    let writer = f.writer(
        vec![Rule {
            operation: Operation::Write,
            call: 1,
            fault: Fault::PauseBefore(gate.clone()),
        }],
        &[],
    );
    std::thread::scope(|scope| {
        let run =
            scope.spawn(|| replicate_manifest_bytes_with_storage(&writer, MANIFEST, &remotes(), 1));
        // One replica write is now parked; the other two must still complete.
        gate.reached.wait();
        let deadline = Instant::now() + Duration::from_secs(10);
        let others_done = loop {
            let done = REMOTES.iter().filter(|r| f.written(r).is_some()).count();
            if done == REMOTES.len() - 1 {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        gate.release.wait();
        let written = run.join().unwrap().unwrap();
        assert!(others_done, "replicas were written sequentially");
        assert_eq!(
            written,
            REMOTES.iter().map(|r| target(r)).collect::<Vec<_>>()
        );
    });
    for remote in REMOTES {
        assert_eq!(f.written(remote).as_deref(), Some(MANIFEST), "{remote}");
    }
}
