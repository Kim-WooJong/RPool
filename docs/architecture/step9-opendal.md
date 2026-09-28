# Step 9 — Optional OpenDAL synthetic prototype

Completed 2026-09-24. Production routes, native encryption gate, manifest formats,
crypt portability code, shared StorageBackend contract and version 0.5.15 are unchanged.

## Dependency decision and evidence

Pinned OpenDAL 0.59.3 (official release 2026-09-22; MSRV 1.91), default-features=false.
Memory is built into core; deprecated services-memory is not enabled. Fs, cloud services,
HTTP client/TLS features and blocking facade are not enabled. Core may still depend on
HTTP data types; this does not mean a cloud service was enabled.
Tokio optional rt-multi-thread, requirement 1.52, lock resolves 1.53.1.

- https://github.com/apache/opendal/releases/tag/v0.59.3
- https://index.crates.io/op/en/opendal
- https://github.com/apache/opendal/blob/v0.59.3/core/Cargo.toml
- https://github.com/apache/opendal/blob/v0.59.3/core/core/src/services/memory/backend.rs

`opendal-prototype` defaults off. Adapter is additionally cfg(test), so enabling the
feature in a normal binary compiles dependencies but provides no production native
route. cargo tree without default features contains neither OpenDAL nor Tokio.
Lock audit: 25 package versions added, none removed or upgraded; dependency lists
changed for rpool, futures-util, uuid and quick-xml. This is additive feature resolution.

## Adapter boundary

Real SDK Memory Operator; private fresh constructor, no arbitrary service/config/root.
One owned Tokio worker; synchronous block_on stays inside adapter. Per-instance admission
serializes operations, checks cancellation/deadline while waiting, rejects same-thread
reentrancy and nested Tokio runtime calls. Drop uses shutdown_background.
I/O chunks 64 KiB, maximum object 8 MiB. Total resident object count is not bounded.
Memory buffers writes until close; source failure, cap violation or pre-close cancellation
aborts buffered content. Commit errors are UnknownOutcome. Arbitrary blocking source/sink
callbacks are cooperative and cannot be forcibly interrupted. Cross-thread callbacks
must not synchronously reenter the same serialized instance.

Ranges truncate at EOF; missing zero-length reads still fail. read_all(Some(n)) returns
at most n. Adapter rejects normalization aliases without changing shared ObjectKey or
legacy strings. Conditional operations, list/native copy/rename are Unsupported;
consistency ProcessLocal, durability Unsupported, atomic replacement Unknown.
Fs deferred: a configured root is not a proven Windows reparse/handle sandbox.
This is synthetic Memory validation, not native encrypted archive/cloud readiness.

## Validation

macOS arm64, Rust/Cargo 1.96 nightly; all final commands --locked --offline --bin rpool:

- cargo test: 147 passed, 0 failed, 10 ignored.
- cargo test --features opendal-prototype: 158 passed, 0 failed, 10 ignored.
- cargo build, with and without feature: passed.
- Existing warnings: 24 test / 60 build, unchanged count.

11 new tests cover isolation, chunked data, EOF/limits, normalization, unsupported flags,
source failure and 8 MiB boundary, staged-write cancellation at EOF, sink failure,
admission deadline/reentrancy, nested runtime/drop, sanitized errors, and real
StorageReader/StorageWriter BLAKE3 verification and corrupted-object rewrite.
Expert SDK/design review found no blocking design flaw; requested staged cancellation,
sink failure and service integration tests were added and passed.
Initial compiler errors (IntoFuture read builder and test formatting) corrected.
Full suite exposed an existing GUI test race: after consuming worker result it assumed
sender disconnection already happened. Test now replays the received result through a
channel to deterministically exercise poll; production GUI behavior is unchanged.

3-OS CI now includes default and optional builds/tests; workflow not published/executed.
Windows/Linux execution, interactive GUI and real rclone/cloud remain unverified.
10 ignored real-tool tests remain ignored. No credentials or cloud access used.
Only existing source changes: storage module wiring and GUI test correction.

Next: Step 10 metadata/crypt/legacy compatibility integration review.
