# Step 3 — MemoryBackend and deterministic fault injection

Date: 2026-09-23. Status at Step 3: source implemented; runtime initially unverified.
Step 4 follow-up: user enabled build/tests; all 13 MemoryBackend tests now pass
on macOS arm64 in the 92-passed/10-ignored suite. See `step4-local.md`.

## Scope and integration

`src/storage/memory/mod.rs` implements the existing `StorageBackend`, not a mock
interface. `faults.rs` wraps `Arc<dyn StorageBackend>` and implements that same
interface. A registry test registers/resolves the concrete backend through
`BackendRegistry`, including duplicate identity rejection. This is ready for
synthetic transfer-service injection; production transfer routing is not changed.

The memory module and its wrapper are gated by `#[cfg(test)]` at the storage
module boundary. They are synthetic tools, not production archive destinations.
No dependency, version, manifest, crypt policy, or existing command is changed.
LocalBackend remains **Step 4**, as specified in `migration.md`; the previous
`pivot-state.json.next_action` incorrectly called it Step 3.

## Semantics

- Exact object keys, no directories or normalization; missing stat/read/delete
  returns `NotFound`, including an empty read of a missing key.
- Immutable byte snapshots allow a read to survive concurrent/reentrant overwrite.
  Source and sink callbacks run outside the store mutex.
- Reads clip at EOF: zero-length, at-EOF and beyond-EOF reads return zero bytes.
  Offsets/end are clamped before conversion to `usize`; range overflow is rejected
  by `ReadRange`. Successful receipts report bytes actually delivered.
- `read_all(Some(n))` returns at most the first n bytes, as the existing trait
  documents. A prefix is **not** proof that complete metadata was retrieved.
  Later metadata callers must check completeness independently.
- Writes stage the full object before atomic publication. Source errors or
  observed precommit cancellation/deadline expiry preserve the previous object.
- Conditional create/update are checked under the commit mutex. Contradictory
  create-with-version options are invalid. Checked, store-wide versions do not
  repeat after delete/recreate. Exhaustion fails without replacing the object.
- Cancellation is cooperative, not an interrupt for a blocked caller callback.
  Known successful commits return success rather than claiming cancellation
  rolled them back.
- Capabilities are process-local; no durability, version-pinned reads,
  conditional delete, list, native copy, or rename is advertised. Those optional
  operations return `Unsupported`. Receipt versions alone do not imply pinning.
- This is an in-memory synthetic backend: object data is retained in RAM and
  writes are staged in RAM. No bounded total memory or persistence is promised.

## Fault schedule

Rules select an operation and one-based invocation index, consumed once.
Invalid combinations, zero indices, and duplicate rules are rejected. Concurrent
call numbering follows entry order; tests must control that order explicitly.
No random faults or sleeps are used. Two-phase barriers pause before delegation
or after commit without retaining schedule/store locks.

- Arbitrary classified pre-operation errors: timeout, permission, not-found,
  corruption, cancellation and other taxonomy variants.
- Corrupted reads flip the first delivered byte without changing stored bytes.
- Short reads deliver a prefix and report the reduced byte count accurately.
- Partial writes fail the source after N consumed bytes: for this atomic backend
  this means **partial staging, not a partially committed destination**.
- Lost write/delete acknowledgements occur after successful delegation and return
  non-retriable `UnknownOutcome`. An optional postcommit gate allows deterministic
  cancellation while the committed outcome is hidden from the caller.

## Verification

13 test functions are written in `src/storage/memory/tests.rs` covering registry
injection, empty objects, overwrite/delete, versions, range boundaries, limits,
CAS and exhaustion, competing CAS, read snapshots during reentrant overwrite,
partial source failure, invocation-index faults, corrupt/short reads, lost
responses, pre/postcommit cancellation, source-time cancellation, deadlines,
sink errors, honest capabilities and invalid rule configuration.

Executed checks: file/module inspection, lexical brace balance, test-source
inventory, JSON parsing and SHA-256 comparison against the pre-Step-3 snapshot.
Only `src/storage/mod.rs` changed among existing Rust files; all other existing
Rust files and Cargo.toml/Cargo.lock are byte-identical to that snapshot.
These checks are **not** Rust parsing, type checking or test execution.

Expert design review contributed commit-time CAS, snapshot callbacks outside
locks, honest unsupported capabilities and the staged-vs-committed fault
distinction. A second static review of the implementation found no blocking
compile-plausibility or correctness issue; it did not compile or run tests.
Barrier tests may hang if a future regression exits a worker before rendezvous;
use an external test-runner timeout when runtime verification is authorized.

No cargo/rustc/build/test, rclone/age, real account or cloud operation was run.
Runtime verification and release readiness remain false.
