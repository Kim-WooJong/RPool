# Native mount IDE performance

## Scope

Version 2.22.1 targets repeated metadata operations from Windows IDEs, Git and
build tools. It does not make a remote drive equivalent to a local SSD. No
namespace, intent, pool or cloud format changes are needed.

- Beginning an edit no longer saves an unchanged namespace when its selected
  ancestry is already durable. The first base is still saved before success;
  failed saves remain retryable. Explicit no-op saves still rotate recovery
  checkpoints, and fsync/seal acknowledgement boundaries are unchanged.
- Random id and spool-directory work runs outside the drive namespace mutex.
- Native cloud read ancestry is remembered only after persistence succeeds.
  Changed observations save and publish in order; hot repeated reads bypass
  both that serialization lock and the namespace lock. Failed saves remain
  retryable. Alternating snapshot readers retain conservative ancestry.
- WinFsp reuses bounded, view-keyed case-folded directory indexes and sorted
  directory listings. Exact names still win and ambiguous folded names remain
  unresolved. Local edits and peer changes invalidate affected cached views;
  results under local-only session pins are not cached. Per-core monotonic
  summary stamps and generation-presence counters catch changes that reverse
  during a build (size A→B→A or a temporary file appearing and disappearing);
  writes on another mount do not invalidate this mount's caches.
- Exact existing paths avoid a component-by-component name-resolution walk;
  directory pages resume after the same bytewise marker without repeatedly
  listing and sorting the directory. FileInfo is built only for emitted entries.

## Reproducible local checks

Run only in a development copy under `projects/rpool`, with an external
`CARGO_TARGET_DIR` and an **absolute, non-symlink TMPDIR**. No real mount, cloud
credentials or cloud mutation is required.

```sh
cargo test --locked --all-targets -- --skip utils::console::tests::writing_to_closed_pipes_does_not_panic
cargo test --locked --bin rpool utils::console::tests::writing_to_closed_pipes_does_not_panic -- --exact
cargo build --release --locked --bins
cargo test --release --locked --bin rpool write_start_benchmark -- --ignored --nocapture
cargo test --release --locked --bin rpool compare_lookup_and_listing_paths -- --ignored --nocapture
RUSTFLAGS='-D warnings' cargo +1.98.1 check --locked --all-targets --target x86_64-pc-windows-gnu
RUSTFLAGS='-D warnings' cargo +1.98.1 check --locked --all-targets --target x86_64-unknown-linux-gnu
```

The existing closed-pipe test changes process-global stdout/stderr descriptors.
Running it concurrently can interrupt libtest or inherited child output; run it
in a separate test process as above. A relative TMPDIR also breaks a recovery
form test that intentionally requires absolute paths. These are harness limits,
not reasons to weaken runtime persistence or path validation.

`virtual_drive/write_bench_tests.rs` uses only 2.22.0 APIs. For an identical
baseline, copy it and its test-module declaration into a detached checkout of
`c01edfb`; do not change production baseline code. Benchmark times are printed,
not timing-based pass gates. Use counts as the stable result.

## Observed local comparison (2026-11-05)

macOS ARM64, release build, local temporary files; no cloud or WinFsp runtime.
The identical write harness ran with 1,000 and 10,000 committed events.

| Workload | 2.22.0 namespace saves | Optimized saves |
|---|---:|---:|
| 50 repeated begins after the first durable base | 50 | 0 |
| 20 native direct saves | 40 | 21 |
| 20 native temporary-file replacements | 100 | 41 |

A separate count-asserting harness over 1,000 committed files observed:

| Query | Previous path | Cached path |
|---|---:|---:|
| 500 folded/negative probes | 1,000 listings | 2 listings |
| 1,000 entries over 17 pages | 17 listings/sorts | 1 listing |
| 500 exact three-component resolves | 3 lookups each | 1 lookup each |

These counts do **not** establish a Windows VSCode speedup ratio. Timings depend
on storage, fsync cost, cache warmth, sync churn and concurrent workload.

## Remaining costs and Windows acceptance

WinFsp's default Fine operation guard still holds an outer exclusive guard
through create and rename callbacks. Removing it would require a separate
supported lifecycle design; the core's prehash/staging already occurs before
its own exclusive namespace section. A regression test checks that narrower
contract, not the absence of WinFsp's outer guard.

Genuinely new ancestry, seals and renames still require durable writes. Existing
file baseline copy, hashing, uncached remote reads and small-file cloud latency
remain. Cache tokens scan open write slots; namespace sync/read-ancestry changes
can clear caches, reducing hit rate. No WinFsp volume caching/flush parameters
were relaxed.

On the affected Windows PC, compare 2.22.1 with 2.22.0 on the same workspace
and cache state: folder-open/indexing time, `git status`, repeated save and
temp-file replacement, and a build producing many small files. Confirm
create/delete/rename, size changes, case ambiguity and peer-PC changes appear
in Explorer and `dir`; verify saved bytes after remount and interrupted-save
recovery. Record these separately from the local synthetic checks.

## Off-lock pool projection (2.22.2)

Status and capacity reporting clone one coherent namespace snapshot, release
StateLock, and reuse a full immutable projection (files and structured conflicts).
Its cache key retains a weak reference to the exact copy-on-write event map plus
the namespace version: same-size edits, same-key mutable access and version
changes invalidate it. Cache mutexes protect only lookup/publication, not full
construction; independent cold readers may duplicate work, but cannot make a
foreground cache lookup wait behind projection construction. Idle sequential
status/capacity calls reuse the result, including when they start from clones.

Upload commit prepares its event, projection, folder state, validation and warm
visible-file projection outside StateLock. An uploader-only commit gate serializes
preparation/install; foreground operations never take this gate. Lock order is
commit gate → state → pins. Every mutable access to StateLock advances a checked
monotonic revision, including unsaved and A→B→A changes. If that revision changes,
a commit rebases only its prepared event and receipt/pending bookkeeping onto the
CURRENT namespace, retaining current folders, pending work, published markers,
bases and durable journal state. Rebase requires exact source event identity and
version, unchanged worker/device and dependency receipt, and exact pending intent
and uploaded content. Ordinary seals/publication/base changes therefore do not
force repeated full projections or starve uploads. Real event/input changes retry
up to eight times, then fail retryably with local data retained; there is never a
projecting-under-lock fallback. Generic Namespace::commit still supports
metadata-only MOVE intents. No format, ancestry, conflict, publication or
spool-release rules change.

Production event mutations fork their projection cache cell (commit, ingest,
import insertion and journal replay). Each old snapshot retains its old cell;
arbitrarily many/late snapshots and failed candidate saves cannot replace the
new live cell. This is branch isolation, not a bounded multi-entry cache. Exact
Shared identity/version remains checked on every lookup. Unchanged event-map
clones still share a cell for idle status/capacity reuse.

Deterministic regression checks (macOS/Linux/Windows core, no mount/cloud):

```sh
cargo test --locked --bin rpool projection_tests
cargo test --locked --bin rpool mount::virtual_drive::visible_tests
cargo test --locked --bin rpool mount::namespace::journal_tests
cargo test --locked --bin rpool mount::virtual_tests
```

The projection hook checks lock availability synchronously; barrier-started
concurrent uploads prove commits preserve memory/disk receipts with exactly one
projection each. A hook mutating unrelated state on EVERY preparation proves
ordinary pending/publication churn cannot starve an upload.
Tests cover coherent capacity IDs/usage/conflicts, idle reuse, same-key cache
invalidation, pending identity/content rejection, retry exhaustion, warm visible
reads and failed-save replay. The ignored baseline-compatible
`projection_foreground_lock_benchmark` reports foreground lock p95/max and
operation duration over 8,000 synthetic events; it has no timing pass gate:

```sh
cargo test --release --locked --bin rpool projection_foreground_lock_benchmark -- --ignored --nocapture
```

Remaining costs: durable save/checkpoint and pending-overlay work still serialize
under StateLock; unrelated mutation paths (e.g. pulled event ingestion and
metadata-only MOVE, history application and import) retain their existing
projection work and are not converted into off-lock transactions.
Synthetic results do not establish Windows mounted latency or live-cloud speedup.

Integration correction: the first full-suite run exposed three account-recovery
failures (`uploaded intent is no longer pending or changed`). Verified Reprocess
adoption legitimately starts an unsealed, unqueued recovery intent and must not
masquerade as uploader completion. A private account-recovery adoption transaction
now rechecks the new destination, intent identity/base and overlapping pending
work under its exclusive workspace's state lock, then commits/saves/installs
metadata without fabricating spool bytes. The strict uploader boundary is
unchanged. Existing replacement verification/no-original-account tests and new
rejection/save-failure/retry regressions cover this separate path.

## Observed lock-latency comparison (2026-11-09)

One matched release-mode run on macOS ARM64, using the identical ignored harness
and 8,000 simple disjoint synthetic events (no user namespace, mounts or cloud):

| Observation | 2.22.1 baseline | 2.22.2 candidate |
|---|---:|---:|
| Three warm pool-status calls, total | 81.784 ms | 0.070 ms |
| Uploaded deletion commit, total | 40.755 ms | 48.700 ms |
| Maximum sampled foreground mutex wait during that commit | 39.257 ms | 4.552 ms |

This is a responsiveness result, **not** improved upload throughput: total commit
time did not improve in this sample. A sampler attempts an acquisition about
every millisecond, so its maximum is observational, not a bound; the candidate
status calls were too short to collect more than one sample. Deterministic
projection-hook and concurrent-mutation tests provide the separate correctness
proof that these target paths build projections outside the state lock. No
Windows mounted latency or reproduction of the user's 33-second stall is claimed
for the candidate.

Validation of the candidate source: 1,212 macOS tests passed (66 ignored),
including the process-global closed-pipe test run separately; warnings-denied
release builds of both binaries passed. Warnings-denied all-target checks passed
for Windows GNU and Linux GNU using Rust 1.98.1. These cross-checks are compilation
only, not Windows/Linux runtime or live-cloud validation. Touched Rust files and
whitespace checks pass; repository-wide fmt/clippy retain unrelated existing debt.
