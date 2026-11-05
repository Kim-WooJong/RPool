# Native mount IDE performance

## Scope

The unreleased change after 2.22.0 targets repeated metadata operations from
Windows IDEs, Git and build tools. It does not make a remote drive equivalent
to a local SSD. No namespace, intent, pool or cloud format changes are needed.

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

On the affected Windows PC, compare the same workspace and cache state against
2.22.0: folder-open/indexing time, `git status`, repeated save and temp-file
replacement, and a build producing many small files. Confirm create/delete/
rename, size changes, case ambiguity and peer-PC changes appear in Explorer and
`dir`; verify saved bytes after remount and interrupted-save recovery. Record
these separately from the local synthetic checks.
