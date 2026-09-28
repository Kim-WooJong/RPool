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

- Quota-aware mount writes: runtime exclusions, alias accounting and conservative parity-aware capacity status in GUI/CLI.
- Explicit unmounted active-archive migration: verified copies, durable receipt resume, atomic reference switch, source/history retention.
- Manifest-only shared successors and completed-upload metadata publication recovery.

- Two-group delayed hedged RS downloads, per-read verified staging and early recovery.
- Bounded configured-remote fairness, reserved hedge capacity and cooperative loser cancellation.
- Slot-free exponential retry backoff; restore-only fallback for exhausted network failures.
- Overlapping data/parity uploads, immediate verified-shard checkpoints, <=2 staged parity groups.
- Optional Resilient placement with backing-alias resolution and parity concentration bounds.
- Persistent local workspace, verified archive writeback, retained old versions.
- Optional encrypted shared-root + worker-name GUI/CLI inputs.
- Immutable causal file revisions; deterministic worker-labelled conflict copies.
- Offline edit/edit and edit/delete preservation, causal deletion/recreation.
- Incoming reconciliation with displaced-byte recovery journal and catalog v2.

## In Progress

No unfinished implementation in the bounded mount quota/capacity/migration task.
Unrelated config-sync/path changes may be present in the working tree; preserve them.

## Next

1. Optional actual-cloud interoperability/outage validation when feasible; no active speed benchmark is required.
2. Validate on actual Windows/WinFsp with two PCs and test cloud accounts.
3. If live incoming replacement while mounted is required, design a filesystem
   write barrier/open-handle-aware integration before removing the current gate.
4. Plan shared-history compaction before deployments exceed exchange limits.

## Known Issues

- Cloud capacity is a conservative full-group estimate using minimum known quota; small-file parity/encryption/metadata/external changes differ. OS statfs remains LOCAL disk capacity.
- Unknown accounting scopes (not only missing numbers) are excluded; known account quota scopes currently cover Drive/OneDrive/Dropbox/Box/pCloud and wrappers.
- Migration switches active workspace archives only, retains original/history bytes and does not relocate shared-root metadata. History warnings remain; no remote GC.
- Partial upload admission credits fully reverified data only; parity is charged again. Eligibility changes may leave additional retained copies.

- Delayed hedging uses passive estimates, not active speed benchmarks or live-cloud measured thresholds.
- Two-group RS staging window; one decoder; single-worker mode has no speculative racing.
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

- RS workers write only private staging files; coordinator alone commits output and resume state.
- Child cancellation retains parent flags/deadline. Canceled slow candidates are not assumed lost.
- One speculative request/group, <=2 globally; total read requests never exceed workers.
- Required parity may borrow one extra per-remote slot if ordinary fairness would strand capacity.
- Two groups and one decoder bound resources; normal data slots reserve hedge headroom.

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

- `src/mount/capacity.rs`: runtime quota eligibility, conservative admission and full-group estimates.
- `src/mount/workspace_capacity.rs`: scoped archive warnings, usage, migration and durable receipt recovery.
- Mount GUI reads atomically refreshed per-run status; identity edits clear stale snapshots.

- `src/commands/get_hedged.rs`: RS-only timer/coordinator, early group completion and candidate retirement.
- `src/storage/reader.rs`, `traits.rs`: progress observation, private staging and composed child context.

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
- `cargo test --locked --bin rpool`: 267 passed, 12 ignored.
- `cargo test --locked --features opendal-prototype --bin rpool`: 278 passed, 12 ignored.
- Default and optional `cargo rustc --locked --release --bin rpool -- -D warnings`: passed.
- 12 new regressions cover quota filtering/recovery/aliases/parity estimates, migration failures/lease/cache/receipt restart, manifest-only shared successors, missing remote metadata completion and CLI maintenance modes.
- `git diff --check`: passed. Synthetic tests do not prove actual clouds or Windows/Linux/FUSE/WinFsp runtime.

## Resume

Quota-aware mount capacity and active-archive migration are implemented, tested and
built on macOS. `docs/MOUNT.md` describes exclusion scope, conservative estimates,
retained originals/history, maintenance CLI and the local OS filesystem limit.
Preserve unrelated config-sync/path worktree edits. No actual user cloud data was
migrated during implementation. Real cloud and Windows/Linux runtime remain unverified.
