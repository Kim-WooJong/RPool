# Project State

Updated: 2026-09-29

## Project

RPool is a Rust GUI/CLI sharded archive tool over encrypted rclone remotes.
Canonical source is this Git checkout (`artifacts/rpool`). Build outputs belong in
`projects/rpool/target`, not in this repository. Windows is the primary intended
GUI platform; current executed tests are macOS.

## Current Status

Native mount M3a (2026-09-30): `src/mount/fs_core/` filesystem core over
VirtualDrive (handles, shared write generations, fsync = local seal ack,
depends_on continuation). 16 fixture traces pass; default suite 505 passed /
21 ignored (non-symlink TMPDIR). No frontend uses it yet. M3b done 2026-09-30
(crash matrix, randomized model traces, stalled uploader; suite 508 passed /
21 ignored). DAV intentionally stays off FsCore. Next: M4 WinFsp, M5 FUSE. Details:
`docs/NATIVE_MOUNT_CRYPT_PLAN.md` "M3a status".

Native crypt M2 (2026-09-29): per-pool `native_crypt` routes `put`/reprocess
shard writes through RPool encryption onto the crypt base, with rclone crypt
readback. Gate unit tests plus an ignored real-rclone local end-to-end test pass;
default suite 489 passed / 21 ignored with a non-symlink TMPDIR (the
`transition_and_mount_share_exclusive_lock_with_explicit_release` test fails
under macOS `/var/folders` TMPDIR because of the symlinked path). No cloud
provider used. Next: M3 filesystem core (`docs/NATIVE_MOUNT_CRYPT_PLAN.md`).

Write-path follow-up (2026-09-29): `docs/MOUNT_WRITE_ROADMAP.md` defines the
measured DAV mitigation and phase-gated native frontend plan. Aggregate DAV
PUT/PATCH/range, write-open, body/copy-byte, seal and incomplete counters now
report on server stop, with bounded progress every 64 seals, without paths or
credentials. A real rclone `serve webdav` VFS test (no OS mount) now compares
eight increasing 32 KiB-prefix frontend PUTs under immediate versus production
60-second write-back. The immediate route sends eight backend PUTs, whereas
the delayed route sends one final 256 KiB PUT and leaves one 256 KiB content
image with exact readback. The production full-cache/write-back selector has
a non-ignored regression test. Synthetic DAV tests show
64 body writes in one open make one revision, whereas 16 acknowledged growing
full PUTs make 16 revisions and 4,456,448 content-spool bytes for a 524,288-byte
final file. Content-Range verifies request-body bytes before sealing, rather
than comparing with total resulting file size. Default suite in an isolated
validation copy (removed after testing): 448 passed / 13 ignored
(including the opt-in VFS test); that test was run separately with installed
rclone. An unrelated config-restore lock test failed once in parallel, then
passed alone and in the full serial suite. The actual native NFS request trace,
VFS crash/power-loss behavior, recursive v7 captured-original accounting and
4GiB cloud roundtrip remain unverified. No same-path blind PUT coalescing was
enabled.

Mac 4GiB real-file follow-up is **blocked**, not validated end-to-end. An
isolated seven-provider v7 resilient 9+3 NFS write reached only about 25MiB
in native VFS before WebDAV write-open revision amplification grew local spool
to 9.2GiB (777 content files; 776 pending intents). The 4GiB random source
was created with SHA-256 recorded, but full cloud publish/readback did not
occur. Stop during active I/O left a retained mount lease and uninterruptible
macOS NFS path/umount processes even after the mount-table entry disappeared.
Do not clear this lease, delete spool/cache, sync the partial intents, or retry
the mount until the OS state is recovered and the write/stop paths are fixed.
No automatic reboot was performed. See
`projects/rpool/4g-e2e-fdb991f6/REPORT.md` for exact evidence and recovery
plan; its temporary rclone credential copy was removed.

Test-artifact cleanup (2026-09-29): disposable validation copies, the completed
read-only Mac mount diagnostic workspace, and `projects/rpool/target/debug`
were removed. `target/release`, the 4GiB test source, uncertain spool/cache,
lease, report, and remote test objects were retained. The mount table has no
RPool entry, but uninterruptible operations on the old 4GiB mount path remain;
do not access or remove that mountpoint before OS recovery.

2026-09-29 follow-up code changes: virtual WebDAV write-back now waits 60s
of inactivity instead of immediate PUT; macOS NFS timeout/unexpected exit
retains an uncertain lease and never forcibly kills an attached NFS server;
old abnormal Mac leases cannot auto-clear. Capacity zero now exposes missing
account/outage declarations. Resilient placement now uses repeatable two-choice
sampling over independent quota domains, choosing lower projected utilization
while preserving quota and max-M-shards-per-failure-domain bounds; full-candidate
greedy remains a safe retry. Local tests pass, but these changes have NOT been
validated with a new actual NFS write or 4GiB cloud roundtrip. Details:
`projects/rpool/fix-20260929/REPORT.md`.

Mac actual-mount diagnostic follow-up: macOS RPool now invokes Homebrew rclone
v1.75.1 `nfsmount` instead of FUSE `mount`; built-in NFS needs no macFUSE kernel
extension. With the attached 7-remote policy in isolated app configuration and
a temporary 0600 rclone config copy (Mikrotik crypt backing path corrected only
in the copy), v7 `--diagnostic-read-only` successfully reached native readiness:
workspace 0.040 s, cloud metadata 13.615 s, NFS startup 0.263 s on final
repeat after the thread-safe macOS mount-table API change. macOS mount table
showed NFS, root directory read succeeded (0 entries),
stop-file unmounted normally (`forced=false`), and no rclone child or mount
lease remained. macOS mount-table lease guard preserves the lease if a residual
native mount survives child exit. Diagnostic mode blocks background sync/GC and
native/WebDAV writes; no remote objects were changed. Temporary credential
config removed. Default suite 439 passed / 12 ignored and warnings-denied
release build passed. File-content reads and populated remote metadata remain
unverified because this Mac's test namespace was empty. Earlier FUSE route
failed due macFUSE extension approval, but is no longer needed for NFS. See
`projects/rpool/mount-latency-cache/REPORT.md`.

Mac metadata-scan latency follow-up: v6/v7 peer event listing now attempts one
8 MiB-bounded `lsjson` per kind/remote and falls back to the previous 16 hash
prefix pages only for a typed output-cap overflow. Other errors remain fatal;
known-history and unseen-event bounds remain intact. On empty diagnostic paths,
six accessible Mac crypt remotes took 6.269 s for one sequential pass versus
94.956 s summed for the old 16-prefix scans; this is not a real mount timing.
Final macOS validation in a now-removed isolated copy: default 435 passed /
12 ignored, optional OpenDAL 446 passed / 12 ignored, warnings-denied release
passed. A transient DAV localhost port rebinding failure passed alone and on
the final full rerun. This earlier scan-only benchmark did not attempt a mount;
the later successful isolated NFS mount and remaining limitations are described above.

Pool profile/workspace evolution follow-up: GUI saves machine-local per-pool mount
profiles and restores workspace/history/cache/options on selection. Compatible
same-membership policy edits use current policy; unfinished incompatible layout
plans remain guarded. Explicit --apply-pool-changes stages a fresh random metadata
epoch in the same named pool, copies/verifies the locally known current/conflict/
sealed-pending view and activates at the same selected workspace path. Original
workspace/history/native cache remain in a sibling backup; history/unseen remote
changes are NOT migrated into the active generation. Other old PCs stay isolated.
No original remote deletion. Sibling lock, durable rename journal, stage ownership
checks and exact v7 committed/pending-plan semantic proofs support interrupted
transition recovery; required bytes missing prevent activation. v6/v7 mode is
preserved; target history limit is journaled. Actual cloud/Windows migration is
unverified. Final local validation in projects: default 433 passed / 12 ignored;
optional 444 passed / 12 ignored; warnings-denied release, fmt and diff passed.
Release CLI exposes --apply-pool-changes. GUI tests include saved profile roundtrip,
pool switching and legacy defaults; transition tests cover interrupted renames,
exclusive locking, pending-write guards and v7 publish-before-commit resumption.

Online mount latency follow-up: required metadata-only bootstrap replaces full
startup upload/GC for normal pool/shared-worker mounts. Quota/reporting and pending
writeback run in one worker after native readiness. Legacy bounded coordinator
bootstrap and replica mode keep their existing behavior. Unmount cancels supervised
remote work/retry waits and joins it without a fresh final cloud sync; pending spool,
VFS cache and uncertain-upload receipts remain. Process cancellation is installed
only in the dedicated virtual-mount CLI child, not the GUI. DAV shutdown joins local
seal/fsync work; local hash/encoding/disk operations mean no hard timing guarantee.
Final validation in projects: default 419 passed / 12 ignored; optional 430 passed /
12 ignored; warnings-denied release, fmt and diff checks passed. Regression coverage
includes metadata-only pending-spool preservation, stop-file cancellation, stalled
read/write child reaping, retry cancellation, uncertain-mutation error precedence,
and immediate remount with an inherited lock descriptor. Adapter lease now explicitly
unlocks on drop. Actual Windows/cloud latency remains unmeasured.

Account-removal follow-up implemented: explicit source-preserving recovery into a
different v6 pool/new workspace permits normal read/write use on remaining accounts.
Optional completed Reprocess plan receipts reuse independently verified replacement
archives without reupload, preserving full source paths; unmatched/current local
writes use fresh verified copies. Never relax original v6/v7 topology/GC guards.
Source is only the locally known v6/v7 view; unseen remote files, old revisions,
dirty native cache and original clean-cache recovery are outside scope. Per-file
failure/resume report is account-recovery.json. Bootstrap is staged then renamed;
source identity/policy and destination edits are checked before resumed work.

Explorer capacity follow-up: unavailable/stale DAV quota now reports known logical
usage + zero verified additional free bytes instead of unsupported properties that
trigger rclone's synthetic 1 PiB fallback. Fresh capacity restores estimates; GUI
and transition logs distinguish unknown quota from a full pool. Startup stage logs
precede cloud synchronization. Actual WinFsp/rclone/cloud behavior is unverified.

Earlier verification used a now-removed isolated account-recovery copy; target/logs under
projects/rpool and test TMPDIR projects/t (short enough for Unix socket paths).
Latest default 433 passed / 12 ignored; optional suite 444 passed / 12 ignored.
Warnings-denied release build, formatting and diff checks passed; release CLI help
exposes the recovery flags. Actual Windows/cloud execution remains unverified.
Independent reviews covered receipt provenance, explicit read exclusion vs auth,
resume revalidation, source preservation and WebDAV quota interoperability.

Version 0.6.0. New opt-in private-snapshot v7 (`--pool-retention` with
`--pool-sync`) enforces current + configured previous content revisions without a
coordinator PC. Positive causal successors own independent verified payloads
before old exact objects are collected. Stable file identity and atomic name
metadata keep committed file/directory MOVE payload-free. Conflict originals and
branches are protected in addition to ordinary history. Direct input originals
are captured locally before write admission, with file/Unix-directory durability.

Requires a NEW workspace and identical fixed policy across PCs. Legacy v6/v5
objects are untouched. Copies need temporary capacity/transfers; causal metadata
and unfinished unpublished uploads remain (no general orphan sweep). Actual cloud,
multiple-PC OS mounts, Windows and power-loss behavior remain unverified.

## Local cache limits (2026-09-29)

Completed GUI/CLI native VFS cache target (`--vfs-cache-gib`, 10 GiB), native
free-space target (`--cache-min-free-gib`, 2 GiB), and GUI exposure of existing
virtual spool budget (64 GiB). Shard cache uses persisted explicit access times,
LRU pre-download eviction, startup trimming after workspace locking, conservative
recovery working-set admission, and error-path cleanup. Windows time updates use
write-capable file handles. A too-small/zero shard budget rejects download
admission; unknown/dirty data is never evicted.

Online-first follow-up: GUI defaults to virtual/on-demand mode and automatic
pool sync. Machine-local cache settings and explicit online/replica preference
persist through Save cache settings or mount/sync launch and restore at GUI
startup; CLI still requires --virtual-drive explicitly. All known namespace
entries remain visible without full local replicas. Existing verified/published
spool reclamation and LRU redownload paths are reused, not rewritten. Legacy
replica workspaces refuse in-place conversion; original local files remain intact.
Explicit cleanup reporting now includes bytes reclaimed during cache startup.

Native limits remain soft for open/dirty files; separate caches/spool add together.
Full replica files, recovery exports and upload staging are outside these targets.
No remote data changed or live mounts started. See docs/MOUNT.md local cache section.

Current follow-up validation: focused cache tests 19 passed; full default suite
399 passed / 12 ignored, optional OpenDAL suite 410 passed / 12 ignored.
Warning-denied release build and formatting/diff checks passed.
Independent reviews verified read leases/publication gates and found
the cleanup-report bug and replica-mode spool-limit label, both corrected.
Real cloud/native Windows/Linux and interactive GUI remain untested.

## Capacity follow-up (2026-09-29)

Latest parity-capacity presentation: Pool and Mount GUI plus `pool capacity`
foreground summed independent account quota × K/(K+M) as coding-only logical
total/remaining, separate from the placement-checked next-file estimate. A
display-only scenario sums distinct unverified backing sections (deduplicating
crypt aliases); it is explicitly conditional and never used for upload admission
or virtual OS quota. Resilient full-stripe group shortfall no longer forces a
false zero upper bound for possible partial stripes. In a projects-side copy,
macOS serial all-target suite **456 passed / 13 ignored**, fmt/diff and stable
1.98.1 warnings-denied release passed; Windows x64 GNU release cross-build
passed. Actual provider quota queries, interactive GUI and native Windows mount
capacity remain unverified. Source changes are restricted to capacity, GUI,
CLI, docs and the necessary BudgetSnapshot constructors.

Heterogeneous quota follow-up: the observed 2 GiB total / 0 B free is caused
by undeclared account identities collapsing independent quotas to their
smallest reported budget and undeclared outage groups blocking Resilient
placement, not by equal-sized RS shards. Pool preview now shows each eligible
backing quota, exclusions, required versus declared outage groups, a direct
path to the identity editor, and an outage-aware upper bound. Resilient
allocation retains its M-shard outage limit and tries a full-candidate
capacity-weighted fallback before the original balance-first feasibility
fallback. The estimate remains a planner-verified bounded lower estimate,
not the sum of account quotas. A 2 TiB account cannot be fully protected by
only a few 2 GiB peers; the remaining logical upper bound is at most the
physical budget outside the largest exclusive outage group.

Implemented workspace-free `pool capacity [NAME]` with read-only overrides and
JSON output, plus asynchronous unsaved GUI pool-policy preview. Shared capacity
calculation distinguishes account quota occupied/total/free, coding-only nominal
upper bound (uncapped), and placement-verified feasible file estimate (bounded).
Aliases stay deduplicated; missing/unverified quota is explicitly incomplete.
Partial-file search checks changing data-shard/diversity boundaries.

Virtual DAV quota reflects current namespace use, reserves queued archive data+
parity, invalidates on synchronization/refresh failure, expires after 120 seconds,
and detects same-size committed replacements. Pending v7 private-copy overhead is
conservatively reported as zero additional space until sync, not guessed. Replica
OS capacity stays local disk space. GUI/CLI use the same calculation; no namespace
usage is invented for pure pool queries. See docs/POOL_CAPACITY.md.

Validation: default 390 / optional 401 tests passed (12 ignored each);
warning-denied release build, formatting/diff checks and CLI help passed.
Local HTTP quota and headless late-GUI-result tests run; actual provider quota and
Windows/Finder/FUSE mount acceptance remain unverified. No cloud data changed.
Previous skew follow-up validation in `projects/rpool/capacity-skew-check`:
macOS default suite **452 passed / 13 ignored**, `cargo fmt --check`,
`git diff --check`, macOS release build, and Windows x64 GNU release cross-build
passed. Synthetic regressions cover 2 GiB + 2 TiB independent/unknown quota,
outage-limited upper bound, weighted fallback avoiding a tiny account when
enough large domains exist, and GUI quota invalidation after identity save.
Windows executable is in `projects/rpool/target/x86_64-pc-windows-gnu/release/`;
actual Windows/cloud runtime and source GUI click path are not verified.

Capacity mode follow-up (v0.7.0): `resilient` retains its declared-outage-group
M-shard concentration bound. New `capacity-first` selects the largest remaining
independent account quota, still charges shared quotas once and keeps K+M
coding, but deliberately does not promise recovery from one whole account or
provider outage. GUI Pool/Upload/Reprocess/Settings and CLI expose the distinct
mode and warning. Metadata is published only to remotes actually used; old
archives and existing frozen workspace policies are not rewritten. Isolated
`projects/rpool/capacity-mode-check` validation: macOS serial default suite
**454 passed / 13 ignored**, fmt/diff checks and macOS release build passed;
Windows x64 GNU release cross-build passed with existing Windows-only
`unused-variables` warning exempted from otherwise warnings-denied build.
The initial parallel test run had one unrelated occupied-DAV-port failure;
serial rerun passed. Real Windows/cloud mount and GUI click-path remain untested.

Provider-health follow-up (v0.7.1): health checks selected crypt remote roots
(`name:`), not Pool/default storage prefixes (`name:rpool`), and deduplicates
the same provider under several paths. Pool/upload roots are unchanged. The
previous `not found: rclone object` display was rclone `lsf` exit 3/4 after
probing missing `rpool` subfolders; an actually missing crypt root still fails.
Windows `sync_parent` no longer warns about its Unix-only `path` argument.
Isolated `projects/rpool/provider-health-check` validation: macOS all-target
serial suite **455 passed / 13 ignored**, fmt/diff checks and macOS plus Windows
x64 GNU release builds passed with `-D warnings -A deprecated`. A fake-rclone
CLI smoke check of `provider health --remote crypt:rpool --json` reported
`crypt:` healthy and retained backing quota. The seven real Windows providers
have not been retested.

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

## Latest Validation — v7 follow-up

Complete. Default 382 / optional 393 tests passed (12 ignored each).
Formatting, diff checks and warning-denied release build passed. No actual cloud
data changed. Test-only journal constructor is excluded from release builds.
Independent review findings fixed: frozen GC proof replay, semantic read/source
resolution, publication acknowledgment recovery, conflicted MOVE rejection,
ready-plan policy validation, and captured-original directory durability.

## Latest Completed Batch

Option C follow-up: keep coordinator-free event sync, add peer-only immutable
shard/group reuse. Implementation, 362 default / 373 optional tests (12 ignored each), formatting
and warning-denied release build passed after final provenance hardening.
Independent bounded review is complete; all identified blockers were fixed. No actual cloud data changed.
That earlier batch is superseded by the opt-in v7 follow-up above.

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

- Recover the old macOS NFS OS state without deleting its lease/spool. Then use a
  NEW isolated workspace for bounded small-write DAV PUT/spool and clean-unmount
  measurement, including new aggregate counters and recursive spool accounting,
  before retrying 4GiB upload/remote readback. The write-back delay is a
  mitigation, not a proven hard bound on amplification. Follow the gates in
  `docs/MOUNT_WRITE_ROADMAP.md` before any server-side coalescing or native write.
- Validate actual Windows/WinFsp and Linux/FUSE with rclone and two PCs; the
  macOS Homebrew rclone is available, but the old NFS test OS state is unsafe
  for a new live mount until recovered.
- On Windows, rerun Providers → Check health for the seven crypt remotes and
  confirm the displayed addresses are `*_crypt:`; investigate any root that
  still fails without treating a missing crypt backing directory as healthy.
- On Windows, declare truthful independent account-budget and correlated
  outage-group IDs, refresh Pool and Mount capacity, then check nonzero DAV/
  Explorer free space and actual shard distribution on a new small test file.
  Compare resilient and capacity-first on a mixed-size pool; never treat the
  latter's K+M parity as provider-outage recovery.
  Never infer account independence from crypt remote names alone.
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

2026-09-29 current write/stop/placement patch, isolated macOS verification
checkout `projects/rpool/fix-20260929`:
- `cargo check --all-targets --locked`: PASS.
- `cargo fmt --check`: PASS.
- `cargo test --all-targets --locked`: **444 passed / 12 ignored**.
- `cargo build --release --locked`: PASS.
- Actual cloud/native 4GiB runtime after patch: NOT RUN. Existing uncertain
  mount/lease/spool remains untouched. A pre-existing `fetch_update`
  deprecation warning appears on the current Rust toolchain.


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

Current write/stop/placement code is locally tested but 4GiB runtime remains
unverified. Read `projects/rpool/fix-20260929/REPORT.md` and the prior
`projects/rpool/4g-e2e-fdb991f6/REPORT.md`; preserve the old test lease,
spool and remote objects. Do not access/reuse its stuck mountpoint or sync
partial intents before OS recovery. Then use a NEW isolated workspace for a
small bounded NFS write test, inspect DAV summary, PUT/revision count and
recursive spool/VFS-cache ratios,
verify clean unmount, and only afterward retry 4GiB and hash readback. Keep
unrelated config-sync/path Git changes untouched. Windows/WinFsp and Linux/FUSE
runtime validation is still outstanding.
