# Project State

Updated: 2026-09-28

## Project

RPool is a Rust GUI/CLI sharded archive tool over encrypted rclone remotes.
Canonical source is this Git checkout (`artifacts/rpool`). Build outputs belong in
`projects/rpool/target`, not in this repository. Windows is the primary intended
GUI platform; current executed tests are macOS.

## Current Status

Version 0.6.0 with batched Unreleased changes. Replica mounts remain the default;
experimental metadata-first virtual mounts are connected and synthetically tested.
Long-term hardening adds lease-aware committed spool cleanup, bounded local writes,
quota-aware Resilient placement and explicit ownership-gated unshared retention.
Shared automatic retention remains UNSOLVED and disabled. No real cloud/multiple-PC/
WinFsp integration validation has been performed here.

## Working

- Quota-aware mount writes: runtime exclusions, alias accounting and conservative parity-aware capacity status in GUI/CLI.
- Explicit unmounted active-archive migration: verified copies, durable receipt resume, atomic reference switch, source/history retention.
- Manifest-only shared successors and completed-upload metadata publication recovery.

- Two-group delayed hedged RS downloads, per-read verified staging and early recovery.
- Bounded configured-remote fairness, reserved hedge capacity and cooperative loser cancellation.
- Slot-free exponential retry backoff; restore-only fallback for exhausted network failures.
- Overlapping data/parity uploads, immediate verified-shard checkpoints, <=2 staged parity groups.
- Optional Resilient placement with declared correlated outage groups and parity concentration bounds.
- Persistent local workspace, verified archive writeback, retained old versions.
- Optional encrypted shared-root + worker-name GUI/CLI inputs.
- Immutable causal file revisions; deterministic worker-labelled conflict copies.
- Offline edit/edit and edit/delete preservation, causal deletion/recreation.
- Incoming reconciliation with displaced-byte recovery journal and catalog v2.

## In Progress

None in this bounded hardening batch. Shared fenced retention remains a known
architectural gap, not completed work. No actual cloud objects or user workspaces
were modified during development/testing.

## Current bounded batch

Implemented the bounded five-lane integration batch (virtual mode opt-in/experimental):
1. Dynamic quota diagnostics, explicit account/outage identities, placement-aware capacity/admission.
2. Virtual-v3 metadata-only namespace and parity-free logical usage.
3. Authenticated stable DAV endpoint, lazy verified shard reads, group-local RS recovery.
4. Live event exchange with conservative revision pinning, lossless conflicts, durable file/folder moves.
5. Durable checkpoint/spool, explicit sealed/partial spool recovery export, safe clean-cache trim.

## Next / runtime gates

- Validate actual Windows/WinFsp and Linux/FUSE with rclone and two PCs; this environment has no rclone binary available at standard locations.
- Test actual dirty native VFS cache replay after process crash. Endpoint/cache identity is stable, but synthetic preservation does not prove native replay.
- True transparent replacement of open native file handles needs a stronger filesystem/client revision protocol.
- Shared remote history compaction needs offline-writer fencing and complete checkpoint/bootstrap protocol; remains disabled. Unshared virtual CLI maintenance is ownership-gated.

## Known limits

- Virtual is opt-in and requires a NEW workspace; separate shared-root/virtual-v3 avoids mixing legacy catalogs/hash semantics.
- Served paths pin a session revision. Incoming updates appear as copies; deletes of served remote paths take effect at remount. Local acknowledged saves/deletes update visibility. Generic writes may conservatively create extra conflict copies.
- Empty directories are local-only. File/folder moves checkpoint together locally, but there is no cross-PC transaction/locking guarantee.
- Capacity is placement-aware but not a reservation. Simulation cap 65,536 physical shards marks lower-bound estimates. Alias/undeclared accounting is conservative; mixed unresolved accounts are excluded until mapped.
- Logical virtual usage is known shared namespace + sealed pending work, not every archive in the cloud. Replica OS space remains local disk; virtual DAV quota is tested over HTTP, not native Explorer.
- Corrupt primary namespace fails closed. Recovery exports spool without rewriting checkpoint; rclone-only dirty cache requires the original mount.
- Migration remains replica active-archive copy/switch, not entire shared history or metadata-root migration.
- Shared and legacy/untracked cloud versions remain retained. New tracked unshared virtual versions can be explicitly reclaimed only with exclusive-ownership acknowledgment and drained caches.
- Shared exchange uses 16 validated hash-prefix pages; 10k/64MiB budget counts unseen events only (8MiB/event). Fresh/long-offline bootstrap exceeding that budget still fails; shared metadata growth remains unbounded.
- Local spool defaults to 64GiB growth budget; VFS cache, staging and exports are separate disk consumers. Pending/unknown/corrupt bytes are never automatically evicted.
- Unresolved conflicts stay protected; conservative generic DAV ancestry may therefore limit reclamation.
- Unrelated config-sync/path edits remain outside this batch.

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
- No automatic/shared remote deletion. Explicit unshared maintenance only sweeps exact newly owned objects, preserves imports/live/conflict/selected history, verifies retained data, journals deletion, and upgrades local namespace to v4 before deletion to reject old binaries.
- Interrupted retention blocks mounting/sync until resumed. Ownership registry and journal are checksummed. Partial/unrecognized spool and detached VFS cache block maintenance.
- Local cleanup advances checkpoint and waits for revision/write leases plus shared publication; dirty spool and recovery data stay protected.
- Quiescent local files are required while unmounted reconciliation runs.

## Architecture

- `src/storage/admin/{domains,budget}.rs`: non-secret account/outage declarations and pooled snapshots.
- `src/mount/{namespace,virtual_drive}.rs`: metadata checkpoints, durable intents, live event sync and recovery export.
- `src/mount/retention.rs`: local reader leases/spool budget, owned archive registry, explicit exact-object GC journal and unshared checkpoint compaction.
- `src/mount/{dav,shard_cache}.rs`: authenticated stable bridge, quota, immutable reads and group-specific verified recovery.

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
- Default tests: 306 passed, 12 ignored.
- Optional `opendal-prototype`: 317 passed, 12 ignored.
- `RUSTFLAGS="-D warnings" cargo build --release --locked`: default passed for this batch; optional feature passed its full test build (optional release not rerun).
- Release binary `mount --help`: retention/exclusive-ownership/keep-previous/spool-budget flags verified.
- `git diff --check`: passed.
- Actual loopback HTTP authentication, PUT durability, range GET, metadata listing and quota exercised without rclone.
- Range cache tests: only intersecting shards fetched; corrupted cache rejected; last nonzero/short RS group recovers without unrelated groups; clean eviction retains unknown/dirty files.
- New regressions cover pinned local save/delete, temp-file MOVE causality, directory MOVE/back after restart, pending directory-case collisions, checksum fail-closed, spool export, stable/occupied endpoint, alias/mixed quota accounting, skewed placement and outage grouping.
- Read-only expert review identified/fixed resume destination quota accounting, mixed undeclared-account overcount, local pin reversion, pending-source deletion causality, MOVE durability and rename-back tombstone ancestry.
- Retention regressions cover active read/write leases, publication failure, pending/corrupt/partial spool, metadata-only directories, quota rejection before write, budgeted local MOVE, interrupted exact-object delete, final persistence boundary replay, v4 checkpoint fencing, imported aliases, corrupt registry and detached VFS cache refusal.
- Resilient regressions cover full-target skipping, alias quota/outage accounting, cumulative groups, short groups and metadata publication excluding unused full targets.
- Shared known-history >10k/64MiB no longer consumes download budget; unknown bootstrap and malformed/duplicate/cross-prefix entries still fail closed.
- No real cloud, native Windows/Linux/FUSE/WinFsp or process-crash dirty VFS replay claim.

## Resume

This hardening batch is validated. Preserve unrelated config-sync/path working-tree
changes. Do not rerun completed work. Shared automatic retention and scalable
cold-client bootstrap need a separate fenced epoch protocol; do not enable TTL
or head-only deletion over the legacy causal event graph. Actual cloud/native
runtime validation remains unavailable. See docs/MOUNT.md for exact CLI ownership,
cache, downgrade and provider-trash boundaries.
