# Coordinator-free shared drive — implementation design

Status: automatic pool metadata/event stage implemented; self-contained snapshots
and safe peer GC remain future work. 2026-09-28.

## Implemented first stage

The user requested existing-pool automatic metadata management first, with a saved
per-file history count for future retention. `--pool-sync` now uses immutable
append-only events replicated to all encrypted pool destinations. Structured
original-preserving projection and GUI conflict lists are connected. History-limit
config is explicitly stored-only; no peer GC or bounded-storage claim is made.
See MOUNT.md for availability, read-fencing and bootstrap limits. The protocol
namespace `events-v6` is intentionally distinct from any future snapshot protocol.


## Required behavior

- No designated PC and no global mutable checkpoint written by competing PCs.
- Different logical paths commit independently.
- A sequential accepted edit replaces the current version.
- Concurrent edits of one base preserve that base at the original path and expose
  every edited branch as `stem_worker+a-<unique-operation-id>.ext`.
- Worker display names need not be unique. Immutable operation/device identity
  disambiguates names; retain portable-name and case-collision validation.
- Keep current versions and unresolved conflict originals/branches, not unlimited
  historical payloads. Upload overlap and resumable deletion remain necessary.
- Offline PCs pull current state. Expired/unverifiable local edits remain recoverable
  locally; they must not revive deleted paths or claim a missing original exists.

## Why virtual-v5 cannot simply lose its coordinator

The single `current.json` pointer and serial admission rely on one writer.
Blind overwrites followed by readback are not compare-and-swap. A late publication
can reference objects another PC has already selected for deletion. Keeping only
base manifests does not preserve base bytes.

The legacy immutable event transport permits independent publishers but retains
all causal history and chooses one edit for the original pathname. Neither its
existing projection nor its retention behavior meets the requested combined goal.

## User constraint: RPool owns metadata in the existing pool

No separate metadata cloud, database service, provider-specific conditional API,
or designated PC is required by the user. RPool must automatically place its
protocol metadata in the existing encrypted storage domain. Asking the user to
choose a metadata provider was an unnecessarily narrow design assumption and is
NOT a blocker.

## Candidate protocol: immutable per-path self-contained snapshots

Architecture/safety review completed. The following self-contained snapshot/GC extension is not implemented yet.

- A PC publishes a unique immutable snapshot for a logical path, not a shared
  global `current.json`. Different files never contend on one namespace pointer.
- Each snapshot carries causal coverage, live revisions, structured conflicts,
  deletion context and private payload ownership. Candidate bytes are uploaded
  and verified before publishing the snapshot metadata.
- All PCs deterministically merge the snapshots they observe. Concurrent
  incomparable snapshots remain live until a successor preserves their state.
- Obsolescence requires a positive causal domination proof from a durable
  successor, never merely absence in a directory listing or a timestamp.
- Successors must own all necessary payload bytes independently. A manifest that
  references another snapshot's deletable archive is insufficient.
- Any PC can retry exact-object collection of proven dominated snapshots. No
  recursive deletion or mutable single-writer checkpoint is inherited from v5.
- Delayed publication/replay must remain suppressed by surviving causal coverage;
  that knowledge cannot be discarded simply because payload cleanup finished.

The distinction between bounded obsolete payloads and causal metadata growth
must remain explicit. A new protocol is needed; disabling the v5 coordinator
check does not implement it. Preserve local bytes on incomplete publication.

## Original-preservation boundary

Ordinary concurrent edits need their common-original bytes, not just its hash or
manifest. Active edits can capture original bytes locally and publish private base
copies with candidate edits. Snapshot joins must preserve the bases needed by
unresolved conflicts before retiring predecessors.

An arbitrarily late multi-generation offline branch may refer to an original
already collected and absent locally. That operation must be quarantined/reported
as original-unavailable, not silently overwrite current state or claim to restore
the original. This respects the earlier decision not to retain cloud history
indefinitely for offline PCs. Exact admission and merge rules are under review.

## Conflict projection and GUI

Structured groups (never filename parsing):

- Logical path and stable group identity.
- Original/base identity and availability.
- Each worker/device, candidate path and revision, or a deletion request.
- Group status and the exact heads/version used for resolution.

UI: a searchable `Conflicts` section with count; expand a logical path to see
Original, Worker A +a, Worker B +a, and deletion rows. Allow copying/opening a
validated materialized path. Do not launch a cloud URI or arbitrary executable.
Resolution must be explicit and concurrency-checked; choosing one branch may
retire other branches only through a defined resolution operation.

For immutable DAG projection, use the unique maximal common content ancestor,
not the oldest ancestor or a hash-elected edited head. Concurrent creation has
no original. Multiple maximal common ancestors require explicit ambiguity and
preservation of all candidate bases. Multi-generation branches retain the common
fork original and each maximal edited head.

## Required validation

- Two PCs, disjoint paths: independent progress and no lost updates.
- One base/two edits: exact original plus both candidate bytes, stable names.
- Same worker labels, Unicode/long names, user-created name collisions.
- Reverse arrival/replay, multi-generation branches, third late branch.
- Delete/edit, concurrent creation, all-head resolution racing a new edit.
- GC racing publication/edit registration; competing collectors.
- Delayed retired upload, expired offline editor, ambiguous commit response.
- Crash after upload and at every metadata/deletion journal boundary.
- GUI shows structured groups and never reports stale resolution as success.
- Real generic-storage publication/list/delete and actual multi-PC/native mount tests,
  separately from synthetic local tests.

## Next phase

Automatic append-only pool metadata management, replica acknowledgement, conflict
projection and config storage are implemented as the requested first stage.
Self-contained snapshots, private payload ownership, safe concurrent GC, bounded
bootstrap and distributed retention-policy agreement remain future work. Never
apply the saved history_limit as a naive truncation of ancestor events/archives.
Existing v5 remains separate; its coordinator cannot be removed by configuration.
