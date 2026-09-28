# Project State

Updated: 2026-09-28

## Project

RPool is a Rust GUI/CLI sharded archive tool over encrypted rclone remotes.
Canonical source is this Git checkout (`artifacts/rpool`). Build outputs belong in
`projects/rpool/target`, not in this repository. Windows is the primary intended
GUI platform; current executed tests are macOS.

## Current Status

Version 0.6.0 with batched Unreleased changes. Replica remains default; virtual GUI
now offers Automatic pool sync, selected by default within virtual mode.
Automatic events-v6 replicates immutable metadata into all existing encrypted pool
roots with no extra shared-root/provider setup and no coordinator PC. Original +
all concurrent worker-labelled branches and structured GUI conflict groups work in
local/synthetic tests. This is the user's requested FIRST stage: history_limit is
saved config only, NOT active peer deletion/compaction. Old payload/metadata growth
is unresolved. Legacy single-coordinator v5 remains separate and unchanged.
No actual cloud/multiple-PC/WinFsp/FUSE acceptance validation performed.

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

## Latest Completed Batch

Option C follow-up: keep coordinator-free event sync, add peer-only immutable
shard/group reuse. Implementation, 362 default / 373 optional tests (12 ignored each), formatting
and warning-denied release build passed after final provenance hardening.
Independent bounded review is complete; all identified blockers were fixed. No actual cloud data changed.
Future self-contained snapshots/GC are not part of this bounded batch.

### Option C implementation

- `src/mount/incremental.rs`: equal-size/layout plain-shard or whole-RS-group reuse;
  fresh group objects via existing upload pipeline, verified composed v2 manifest.
- Checksummed durable source/base/policy recipe, subgroup receipts and exact final
  manifest prevent retry identity drift. Full-upload route receipt prevents
  switching upload strategies after a partial/ambiguous full publication.
- Captured sole parent only, with this workspace's matching upload identity and
  local commit receipt (metadata-only rename receipts do not qualify). Imported
  and first remote-derived edits, changed layouts and Resilient use full upload.
- Composed manifests bypass exclusive ownership; shared GC remains blocked.
  Destructive provider drain rejects virtual archives, including parent archives.
- Existing original+worker-branches/GUI conflicts remain unchanged. No central CAS,
  no automatic chunk merge, no remote-durable application fsync claim.
- History-limit remains stored-only, not enforced. Payload duplication is reduced,
  but metadata/changed-group history still accumulates; verification reads remain.


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
- Validate virtual-v5 against real cloud eventual-consistency behavior and native cached writes. It requires one designated coordinator and new workspaces; no election/takeover or automatic v3 adoption.

## Known limits

- Automatic pool events-v6 is append-only: old cloud versions and causal records are retained. `pool-sync-config.json` history_limit (0–10000, default 0) is reserved for future safe per-file retention, not enforced.
- All configured metadata replicas must be readable/writable for successful sync/ack. Partial publication retries; downloaded metadata heals missing replicas before acknowledgement. No degraded offline commit.
- Initial unseen metadata budget 10k events / 64 MiB per replica and 64 MiB unique combined; no peer compaction/checkpoint bootstrap yet.
- Pool name defines auto namespace scope within actual destinations. Rename changes scope; consistent portable configuration/keys is required. Existing workspace binds full roots and rejects topology/root changes; no automatic migration.
- Peer files already read in a mount are fenced if their revision changes; remount is required, avoiding mixed unconditioned range reads. Native VFS cache is isolated intact on restart. GUI conflict view has no all-head resolution button.

- Virtual is opt-in and requires a NEW workspace; separate shared-root/virtual-v3 avoids mixing legacy catalogs/hash semantics.
- Legacy virtual-v3 served paths pin a session revision. Incoming updates appear as copies; deletes of served remote paths take effect at remount. Local acknowledged saves/deletes update visibility. Generic writes may conservatively create extra conflict copies.
- Empty directories are local-only. File/folder moves checkpoint together locally, but there is no cross-PC transaction/locking guarantee.
- Capacity is placement-aware but not a reservation. Simulation cap 65,536 physical shards marks lower-bound estimates. Alias/undeclared accounting is conservative; mixed unresolved accounts are excluded until mapped.
- Logical virtual usage is known shared namespace + sealed pending work, not every archive in the cloud. Replica OS space remains local disk; virtual DAV quota is tested over HTTP, not native Explorer.
- Corrupt primary namespace fails closed. Recovery exports spool without rewriting checkpoint; rclone-only dirty cache requires the original mount.
- Migration remains replica active-archive copy/switch, not entire shared history or metadata-root migration.
- Legacy/shared-v3 and imported/untracked archives remain retained. Bounded-v5 exact owned versions are pruned automatically by one coordinator. Unshared explicit retention remains ownership-gated.
- Legacy shared exchange uses 16 validated hash-prefix pages; 10k/64MiB budget counts unseen events only (8MiB/event). Fresh/long-offline bootstrap exceeding that budget still fails; shared metadata growth remains unbounded.
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
- Legacy modes have no automatic/shared remote deletion. Explicit unshared maintenance only sweeps exact newly owned objects, preserves imports/live/conflict/selected history, verifies retained data, journals deletion, and upgrades local namespace to v4 before deletion to reject old binaries.
- Interrupted retention blocks mounting/sync until resumed. Ownership registry and journal are checksummed. Partial/unrecognized spool and detached VFS cache block maintenance.
- Local cleanup advances checkpoint and waits for revision/write leases plus shared publication; dirty spool and recovery data stay protected.
- Quiescent local files are required while unmounted reconciliation runs.

## Bounded shared-v5 decisions

- User explicitly prioritizes bounded cloud storage over indefinitely protecting old offline PCs.
- Opt-in GUI/CLI (`--bounded-shared`, coordinator `--shared-coordinator`, default `--shared-keep-previous 0` (latest-only)). NEW workspace + `virtual-v5` remote subroot and binding/namespace5 fence old clients.
- Exactly one externally designated coordinator. No generic-rclone CAS, election or takeover; never clone coordinator workspace. Workers publish requests and await positive checkpoint receipts before local cleanup.
- Latest only by default; explicitly configured N previous versions remain optional. Bounded mounts require successful initial cloud synchronization before exposing DAV/native mount; errors retain local work and stop startup. File bytes remain on-demand downloads, with periodic eventual reconciliation while mounted.
- Latest + N previous per live path. Delete retires all owned history for that path. Live conflicts protected. Replacement requires old/new overlap space; provider trash can delay release.
- Whole current checkpoints replace causal-log replay. Coordinator durable transition journal activates/readbacks before exact sweep; every pointer observation checks durable high-water before writes/deletes.
- Unique per-attempt upload ledgers before writing, old unfinished attempts reswept for late writes. Protected archive ledgers retain alternate-layout references until retirement. Small unfinished ledger metadata can accumulate under unlimited failures.
- Stale unsynced chains export locally; mismatching old deletes cannot remove current files. Multiple queued own creates survive own acknowledgement advances. MOVE destination must be accepted before deleting source.
- Expired remote pins are dropped. Actual prior read bases remain conservative, explicit fresh reads and own acknowledged writes advance them. Late reads cannot repin retired cloud/local revisions.
- Previous native VFS cache moves intact to `recovered-native-cache/<id>` on bounded mount restart; it is not blindly replayed. Recovery directories require manual inspection and consume local disk.
- Imported archives remain nonowned. No automatic migration/deletion of legacy history. Checkpoint/list safety limits still bound live namespace scale (64MiB), not arbitrary files.

## Architecture

- `src/mount/pool_sync.rs`: automatic destination derivation, all-replica immutable metadata collect/publish, durable future history config and status sidecar.
- `src/mount/peer_projection.rs`: validated event-DAG projection, maximal common originals, worker+a aliases, explicit delete/ambiguous groups.
- Namespace version 6 / `events-v6` is append-only and distinct from the future self-contained snapshot protocol. Mount GUI reads pool-sync-status.json independently of quota results.

- `src/mount/shared_checkpoint{,_model,_transport,_tests}.rs`: bounded shared-v5 runtime, proposal/checkpoint reducer, monotonic activation+GC journal, synthetic regressions.

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

2026-09-28 Option C incremental-upload batch, macOS:
- Default tests: 362 passed, 12 ignored; optional OpenDAL: 373 passed, 12 ignored.
- Changed RS/plain group upload counts, borrowed object identity preservation,
  unchanged metadata-only upload, corrupt references, exact-byte publication retry,
  source/policy/checksum mismatch fail-closed, fallback, captured ancestry and
  imported→rename→edit provenance rejection tested.
- Real Reed–Solomon parity with one missing data object in each of a borrowed and
  a new group restored exact full bytes through production ShardCache/StorageReader.
- Warning-denied default release, cargo fmt --check and git diff --check passed.
- Independent review found and fixed exclusive-ownership contamination risk,
  destructive provider-drain references, externally managed imports, and local
  metadata-only renames incorrectly qualifying as upload provenance.
- Tests use local/injected backends; no real cloud/native multi-PC acceptance test.

2026-09-28 automatic pool metadata first stage, macOS:
- Default tests: 351 passed, 12 ignored; optional OpenDAL: 362 passed, 12 ignored.
- Includes replica failure/retry/healing, deterministic pool paths, persisted history config, original+both sibling edits, deleted-path directory reuse, mixed maximal-base ambiguity, offline late sibling, resolution race projection, pending-read fences and real loopback HTTP cross-request range fencing.
- Independent reviewers identified/fixed scope identity depending on membership, non-atomic worker label persistence, deleted-path name reservation, common-base ambiguity and read-baseline/range hazards.
- Default release build with RUSTFLAGS="-D warnings", cargo fmt --check and git diff --check passed. Release CLI help confirms automatic pool/worker/history options.
- GUI status wiring compiled; interactive GUI and real rclone/cloud/native mounts not exercised. No remote deletion run.

2026-09-28 latest-only follow-up, macOS:
- Default tests: 336 passed, 12 ignored; optional OpenDAL: 347 passed, 12 ignored.
- Default release with RUSTFLAGS="-D warnings" passed. cargo fmt --check and git diff --check passed.
- Independent diff review found no material issues. Native startup failure gate reviewed in code, not exercised with rclone/cloud.

2026-09-28, macOS, bounded shared-v5 batch:
- Default: **333 passed**, 12 ignored. Optional OpenDAL: **344 passed**, 12 ignored.
- New checkpoint/adapter regressions: retention over 120 versions, stalled/late upload ledgers, stale edit chains, accepted vs published state, local reader leases, multi-file queue, cloud/directory MOVE prerequisites, retired-read races, rollback-before-GC, native cache isolation, unknown pointer outcome and GC retry.
- Expert reviews found and fixed stale cloud/local repinning, pending-chain/queued-create expiry, upload retry/alternate-layout ledger tracking, GC scan starvation and monotonic pointer checks inside coordination. Final checks also preserve literal pre-v5 namespace checksums and intentional recreation after an acknowledged local deletion; locally detected stale writes never upload new conflict files.
- Default release build with `RUSTFLAGS="-D warnings"` passed; final `mount --help` confirms bounded/coordinator/previous-version options. `git diff --check` passed. Optional feature passed its full test build; optional release was not rerun.

Previous unshared hardening batch (macOS):
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

The user selected Option C: coordinator-free metadata sync plus immutable data
reuse. This bounded incremental-upload batch is complete; stop. Do not imply
history_limit triggers deletion or that coordinator-free bounded cloud storage is complete. Future requirements:
self-contained snapshots/private ownership, peer compaction/GC and agreed retention
policy; see docs/PEER_SYNC_DESIGN.md. Do not ask for a separate metadata provider or
reintroduce a designated PC. Preserve unrelated config-sync/path changes.
