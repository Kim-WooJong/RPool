# Online drive write path and native frontend plan

Updated: 2026-09-29. This is an implementation plan, not a claim that the
amplification or the 4 GiB roundtrip has already been fixed.

## Goal and current evidence

For one identifiable logical write generation, make retained transient bytes
and published revisions independent of the number of OS write callbacks. Keep
every independently acknowledged edit, v7 causal branch/original, reader-pinned
revision, and unsynced byte. Cloud replication remains asynchronous and distinct
from local save acknowledgement.

The current virtual mount is OS NFS/WinFsp/FUSE through rclone VFS, then a
loopback WebDAV server, then RPool's immutable spool and cloud transport.
`--vfs-write-back 60s` is already enabled for this route. One DAV write open
creates one intent and a successful flush seals it; body fragments of the same
PUT do not each create a revision. Ordinary PUT truncates, so it does not copy
the previous file on open. PATCH/Content-Range can copy the baseline. The
previous Mac attempt ended near 25 MiB with 777 content files and 9.2 GiB of
spool, a pattern consistent with repeated full-prefix PUTs, but the actual
HTTP/VFS request sequence was not captured. The 60-second delay is a debounce,
not a guaranteed per-save boundary or a proved power-loss durability contract.

The old 4 GiB test mount, lease, spool, and VFS cache are retained. No new Mac
mount may use that workspace. Recover the old host's uncertain NFS state before
fresh mount tests; do not delete its dirty cache or partial intents.

## Phase 0 — establish the write trace (current)

1. Count authenticated PUT/PATCH/Content-Range attempts and successful PUTs,
   DAV write opens (truncate vs non-truncate), accepted body bytes, baseline
   copy bytes, completed seals, and incomplete bodies. Log aggregate counters
   only; never paths, tokens, request bodies, or headers. Separate actual
   recursive spool bytes (including v7 `captured/` originals), allocated disk
   blocks, pending count, and rclone VFS cache bytes. The existing `spool_bytes()`
   limit counts only each intent's top-level `content`; do not use it alone as
   total-space evidence.
2. In a disposable workspace with no real remotes, verify that many callbacks
   within one PUT produce one seal, while multiple successful full-prefix PUTs
   produce distinct acknowledged revisions. This is a characterization, not a
   claim that independent PUTs should be merged.
3. After the old Mac NFS condition is confirmed recovered, use a **new**
   workspace/cache/lease/mountpoint for a 32 KiB-chunk 4 GiB write. Record the
   counters, recursive spool and VFS-cache trajectories, final size/hash,
   stop behavior, and the exact request sequence. Repeat with a pause longer
   than 60 seconds. A direct `rclone copyto` is not a substitute for this NFS
   VFS path.

Exit gate: identify whether the growth comes from repeated full PUTs, baseline
copies, v7 captured originals, or a combination. No unsafe server-side
same-path coalescing before this gate.

## Phase 1 — remove avoidable amplification on the existing frontend

**Case A: one uninterrupted native write yields one complete PUT.** Keep the
VFS coalescing boundary. Verify the exact rclone cache fsync/restart contract,
then qualify 60 seconds or tune it from trace data. A save may reside only in
the VFS cache during that interval; UI/docs must not say RPool's spool already
contains it. Reopen and pause-across-debounce tests must preserve all final
bytes. This removes per-callback revision amplification for the qualified
continuous workload, not for arbitrarily many distinct saves.

**Case B: one logical write still yields many complete PUTs.** First obtain a
trusted generation/continuation identity from the frontend, or a documented
single-writer editing-stream contract. Path equality, timing, and identical
content are not enough. If that identity cannot be obtained from rclone/DAV,
do not merge blind PUTs; advance the shared core and native frontend instead.

If server-side staging is justified, implement it separately from existing
immutable pending intents:

- Stream each full PUT into a fresh private candidate. After complete-body
  validation, fsync its bytes and directory, then atomically publish a durable
  staged-checkpoint pointer **before** acknowledging success. A previous
  acknowledged checkpoint stays authoritative on failure. It may be reclaimed
  only after durable supersession and proof of no readers/uploads/dependencies.
- Freeze a staged checkpoint into an immutable pending intent on explicit
  generation end, required read/delete/MOVE barrier, sync drain, or verified
  restart recovery. Once frozen, never mutate its bytes or upload plan. A
  proven continuation after freeze is an explicit causal successor; unrelated
  writes remain sibling edits. Sync claims frozen data before copying pending.
- Per-path operation locks cover open through acknowledgement and coordinate
  reads, deletion, MOVE, freeze, and upload claim. Establish lock ordering and
  atomic resolve-and-pin reads before enabling staging. Preserve v7 ancestry,
  captured originals, unresolved conflicts, and existing refusal to MOVE with
  pending edits.
- Range writes need copy-on-write or a transactional log so a crash cannot
  damage the old acknowledged image. Do not implement them by in-place mutation
  of a checkpoint. Add byte/count admission accounting for content, captured
  originals, and candidates; budget rejection retains the old checkpoint.
- Add a persisted format/protocol version and explicit rollback/migration rule.
  Older binaries must not silently ignore staged records.

Exit gate: a failing-before/passing-after replay of the **observed** trace;
bounded extra bytes per identified generation, exact final hash, no accidental
merging of two writers, crash recovery at every publication boundary, pinned
reader and concurrent-upload stability. Preserve accepted writes on quota
failure. Keep the DAV route available while qualifying changes.

## Phase 2 — protocol-independent filesystem core

Build stable file identity plus explicit handle/write-generation APIs:
`lookup`, `open`, `read-at`, `write-at`, `truncate`, `flush/fsync`, `release`,
`rename`, `delete`, and `freeze`. Specify which operation acknowledges local
durability and which merely starts cloud sync. Keep immutable reader revisions,
v7 causal parents, and atomic metadata MOVE. A native adapter must not bypass
the same quota, retention, upload, and recovery rules. Use protocol-independent
operation traces and fault injection before connecting an OS frontend.

Exit gate: concurrent writers, old readers, rename-over, delete/recreate,
short/incomplete writes, retried operations, abrupt process exit, and stalled
uploader all preserve acknowledged data without mutating published content.

## Phase 3 — native OS frontends, then rollout

The replacement is for **rclone mount/VFS + loopback DAV**, not necessarily
for rclone cloud transfers. Add opt-in read-only adapters first, then writable:

| OS | Candidate | Contract to settle before writes |
| --- | --- | --- |
| macOS | RPool local NFS server using built-in client | NFS version, stable WRITE/COMMIT, verifier/restart, duplicate RPCs, persistent filehandles; NFSv3 has no CLOSE. |
| Windows | WinFsp | Flush vs cleanup/close, cached/paging I/O, sharing, rename and delete-on-close. |
| Linux | FUSE | Repeated flush, fsync vs release, writeback-cache ordering and truncation. |

Never mount DAV and native writable frontends on one workspace concurrently.
Keep frontend selection explicit during rollout. For each actual OS, verify
read-only listing/read/stop first; then small writes, remount/recovery, competing
workspaces, and an independent hash readback. Finally repeat the fresh 4 GiB
copy with 9+3 placement, publish to real cloud, and read it from a separate
workspace. Measure latency, cumulative bytes written, peak/retained spool,
VFS/native cache, remote traffic, and clean/uncertain stop independently.
Only after those gates may native become default or DAV support be retired.

## Roles and acceptance boundaries

`main` owns the trace, code integration, source/work-copy separation, test runs,
and final verification. Expert architecture work reviews the staging durability,
lock ordering, v7 causality, and native OS contracts; an independent expert
reviews crash/conflict regression evidence before a frontend default switch.

Current safe acceptance is narrow: a single completed new-file PUT gives one
pending content image; independently successful PUTs remain distinct until a
trusted continuation contract exists. A 4 GiB full cloud roundtrip, power-loss
durability, real multi-PC mount behavior, and native frontend throughput are
**not yet validated**. No cleanup of the old Mac test data and no automatic
remote deletion or migration is part of this plan.
