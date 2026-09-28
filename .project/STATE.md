# Project State

Updated: 2026-09-28

## Project

RPool is a Rust GUI/CLI sharded archive tool over encrypted rclone remotes.
Canonical source is this Git checkout (`artifacts/rpool`). Build outputs belong in
`projects/rpool/target`, not in this repository. Windows is the primary intended
GUI platform; current executed tests are macOS.

## Current Status

Version 0.6.0 with batched Unreleased changes. Local-first writable mounts and
optional shared revision exchange are implemented and synthetically tested.
No real cloud/multiple-PC/WinFsp integration validation has been performed here.

## Working

- Cross-group data download scheduling, per-configured-remote fairness and bounded global tasks.
- Slot-free exponential retry backoff; restore-only fallback for exhausted network failures.
- Overlapping data/parity uploads, immediate verified-shard checkpoints, <=2 staged parity groups.
- Optional Resilient placement with backing-alias resolution and parity concentration bounds.
- Persistent local workspace, verified archive writeback, retained old versions.
- Optional encrypted shared-root + worker-name GUI/CLI inputs.
- Immutable causal file revisions; deterministic worker-labelled conflict copies.
- Offline edit/edit and edit/delete preservation, causal deletion/recreation.
- Incoming reconciliation with displaced-byte recovery journal and catalog v2.

## In Progress

No unfinished implementation in this bounded transfer scheduling/placement update.
Unrelated config-sync/path changes may be present in the working tree; preserve them.

## Next

1. Measure 1-vs-N-worker large-file transfers and provider outages on actual test clouds.
2. Validate on actual Windows/WinFsp with two PCs and test cloud accounts.
3. If live incoming replacement while mounted is required, design a filesystem
   write barrier/open-handle-aware integration before removing the current gate.
4. Plan shared-history compaction before deployments exceed exchange limits.

## Known Issues

- No live-cloud speed/outage validation; no speculative fastest-K/cancellation of slow reads.
- Direct reads finish before recovery; damaged groups decode sequentially with parallel parity fetch.
- Scheduler limits are per operation and configured remote name, not across processes or proven accounts.
- Resilient guards one resolved backing target within M; independent provider failure domains remain unknown.
- Existing Pool/workspace policies stay unchanged. Older binaries cannot load the new Resilient enum.

- Mounted mode publishes/fetches revisions only. Incoming files/deletions require
  unmounted reconciliation with no remaining VFS cache files or mount lease.
- Case/prefix collisions of live paths fail closed; no silent overwrite.
- Empty directories remain local-only. Rename is new path plus deletion.
- No history/recovery GC; exchange limits: 10k events, 8 MiB/event, 64 MiB total.
- Shared metadata root is a separate availability dependency. All PCs need the
  appropriate crypt keys and compatible remote aliases for archive manifests.

## Decisions

- Missing/corrupt classification for mutation reuse and upload journals remains unchanged.
- Only restore accepts exhausted timeout/transient/rate-limit failures as parity candidates.
- Local-output/auth/cancel errors are terminal; failed readback cannot replay acknowledged writes.
- Resumed Resilient plans revalidate current backing mappings without changing stored assignments.
- Parity is regenerated on resume because source bytes may change; reuse needs exact generated hash.

- Conflict outcome is causal, not timestamp-based last-writer-wins.
- Preserve both concurrent file versions and retain an edit concurrent with deletion.
- Each PC owns its own workspace/cache. Never share/copy an active workspace.
- No remote deletion and no automatic deletion of local recovery/cache data.
- Quiescent local files are required while unmounted reconciliation runs.

## Architecture

- `src/storage/scheduler.rs`: bounded round-robin dispatch, delayed retries, dynamic parity jobs.
- `src/commands/put.rs`, `get.rs`: verified scheduler/journal integration.
- `src/placement/assign.rs`: balanced assignment and strict resolved-target concentration.
- README "Strict placement and fair transfers": operating limits and activation.

- `src/mount/shared_model.rs`: validated immutable events and deterministic reducer.
- `src/mount/shared_transport.rs`: encrypted content-addressed event exchange.
- `src/mount/workspace_shared.rs`: local baselines, publication, incoming apply/recovery.
- `src/mount/workspace.rs`: local catalog, snapshots and verified archives.
- `src/mount/adapter.rs`: owned rclone mount/cache lifecycle.
- `docs/MOUNT.md`: user-facing usage, boundaries and recovery guidance.

## Validation

2026-09-28, macOS:
- `cargo test --locked --bin rpool`: 242 passed, 12 ignored.
- `cargo test --locked --features opendal-prototype --bin rpool`: 253 passed, 12 ignored.
- Default and optional `cargo rustc --locked --release --bin rpool -- -D warnings`: passed.
- 16 added regressions: dispatch limits/fairness/dynamic tails, cross-group progress, network parity fallback, output failure refusal, multi-group roundtrip, alias collapse/resume and readback replay refusal.
- Synthetic behavior is not real cloud/WinFsp/FUSE proof.

## Resume

Transfer scheduling and optional Resilient placement are implemented and validated
on macOS with synthetic backends. Read README "Strict placement and fair transfers"
for bounds. Next useful validation is controlled live-cloud throughput/outage tests.
Preserve unrelated config-sync/path edits; do not automatically change live Pool
policies. Existing workspaces freeze their policy and need explicit new-workspace
selection/reprocessing to adopt a different layout.
