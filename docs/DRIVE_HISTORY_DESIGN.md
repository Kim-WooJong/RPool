# Drive history: trash, file versions, rollback

Status: implemented (backend + CLI, `src/drive_history/`). GUI reads the JSON
contract in `src/drive_history/model.rs` (or calls `drive_history::api`).

## What the formats already keep

| | v6 pool sync (GUI default) | v7 private snapshots |
|---|---|---|
| History records | One immutable event per change (`Event {path, parents, content}`; `content: None` = delete). Never deleted; gated metadata compaction moves them into checkpoints, losslessly. | Snapshots keep **every** semantic revision of a file as causal evidence (plus name records); never deleted except by the same gated compaction. |
| Bytes | Never collected ("no shared GC"). Every revision's manifest stays readable. Incremental uploads may share shard objects between revisions. | Only the `history_limit` closure is retained by the frontier; a retired snapshot's payload objects are deleted once a successor dominates it (any PC may run that GC). |
| Time | No timestamp in `Event`. | No timestamp in `Snapshot`/`Revision`. |

So the history itself (who changed what, ancestry, deletions) is always
complete; what can expire is **v7 bytes**.

## Times (format decision)

Adding a time field to `Event` (or `Snapshot`) is **not** compatible: ids are
`blake3(serde_json(record))`; an older RPool drops the unknown field when it
parses, recomputes a different id and rejects the record ("identity
mismatch"). Older RPool must keep reading new records, so no record format
changes. Instead the **object ModTime of the record in the cloud listing** is
used (`times.rs`: one recursive `lsjson` per replica, earliest time over
replicas). It is the upload time, identical on every PC, and also exists for
records written by older RPool. Consequences:

- Local unpublished records count as "now".
- Records only present in a checkpoint (their listing deleted after the
  compaction grace period, ≥ 14 days after being checkpointed) have no time:
  shown as unknown, treated as *older than any requested time* by rollback
  (which is true: compaction only deletes old records), and never expire.
- Times only move forward along ancestry (`effective time = max(own,
  parents)`), so filtering "at time T" keeps every ancestor.

## Model

`graph::History` normalizes both formats: revisions with lineage (v6: event
path; v7: stable file id, so v7 history follows moves), parents, content
(hash, size, restorable), author (worker), time; a projection "what is visible
at time T" (v6: the drive's own `peer_projection::project` over the events up
to T; v7: the snapshot materialization rule: one head at its name, several
heads = common original + labelled copies). Trash, versions and rollback are
pure functions over this graph (unit tested with fake graphs).

- **Trash**: lineages whose every head is a deletion. Entry id = deletion
  revision; bytes = nearest earlier content. Hidden: purged, expired, and
  deletions whose bytes are visible elsewhere (moves, restored copies,
  identical copies); identical deleted bytes are listed once (newest).
  `dir:/path` aggregates a gone folder holding ≥ 2 trashed files.
- **Versions**: every revision of the lineage incl. deletions and conflict
  branches, newest first (effective time, then causal depth). v6 also follows
  a move (new path whose first revision has the deleted old path's bytes).
- **Rollback to T** (scope = folder or `/`): diff of visible files at T and
  now: Revert (other bytes now), Undelete (gone now), Remove (created after
  T → trash). Paths whose bytes are no longer available are `skipped`.

Every operation **publishes new revisions** (normal events/snapshots older
RPool reads as ordinary edits): v6 restore references the old content again
(one new event descending from what is at the path now, exactly like a move;
no bytes move); v7 restore reads the old payload and writes it as a new local
version (uploaded by the next sync as this workspace's own payload, as v7
ownership requires). Remove = ordinary delete. Rollback is therefore itself
history; rolling back to a time just before it undoes it (tested).

## Retention and data lifetime

`Retention {trash_days 30, keep_versions 20, version_days 90}` (0 =
unlimited) is stored per pool in the pool store (`pools.json` → `retention`
map; portable via export/import; older RPool ignores the field). Missing entry =
defaults.

- Listing: trash entries leave the trash at `deleted + trash_days`.
- `retention::protected`: bytes still needed = last content of every
  non-purged, non-expired trash entry plus its versions, and per live file the
  newest `keep_versions` previous versions replaced less than `version_days`
  ago. A missing time never counts as expired.
- **v7 GC guard** (`mount/peer_snapshot_history.rs`): before a retired
  snapshot is journaled for GC, if it holds protected bytes that no frontier
  snapshot retains, its GC is deferred (one holder per revision) and
  re-evaluated every sync; once retention no longer needs it, GC proceeds
  normally. Started GC journals always resume. Expiry uses the time this
  workspace first saw each record (`drive-history-seen.json`) — no listing in
  the sync path; a fresh workspace protects for a full period. Errors fail
  closed (defer). The guard is active only when retention was **explicitly
  set** for the pool (`rpool drive retention set`); otherwise the snapshot
  `history_limit` alone decides, unchanged. Read at drive open (remount after
  changing it).
- **v6**: nothing is ever deleted. Expiry and purge only hide entries and make
  data *eligible*: `trash purge` reports `eligible_bytes` (shard objects
  referenced only by non-kept, non-current revisions — shared incremental
  objects are counted as kept). Physical deletion is deliberately **not
  automated**: events are permanent drive metadata references, so the pool
  migration retire/fossil machinery (which treats any drive metadata
  reference as keeping data) would refuse them, and any PC may still restore
  an old revision by reference. A future guarded cleanup must quarantine via
  a published marker, wait a grace period for every PC, and re-check
  references (like `migration/retire`).

Purge (`trash purge --id … | --expired`, `trash empty --confirm`) publishes an
immutable, content-addressed **mark** `{format:1, kind:"purge", ids, worker,
unix}` under `<metadata root>/history/events/<blake3>.json` on every replica of
the current generation. Older RPool never lists `history/`. Marks hide the
entries on every PC and end their retention protection; they delete nothing.

## Where it runs

1. **Mounted** (this PC's mount registry lists a v6/v7 mount of the pool or of
   `--workspace`): the CLI writes `<workspace>/drive-history/requests/<id>.json`
   (atomic rename); the mount's maintenance loop polls every 2 s, renames it to
   `.working`, runs it on its own `VirtualDrive`, answers in `responses/`. Only
   the mount may touch the locked workspace, and its namespace/pins/sync see
   the change immediately. An older mount never answers: the client times out
   and withdraws its unclaimed request (nothing runs later by surprise).
2. **Unmounted `--workspace`**: opened directly (lock, like a mount); changes
   publish as that workspace's worker.
3. **No workspace**: listing/preview read the cloud read-only like
   `pool browse` (v6 `metadata_pool::read_v6` incl. checkpoints; v7 through a
   throwaway workspace whose pull only lists/reads); purge publishes the mark;
   restore/rollback run in a scratch workspace on the newest generation
   (`<config>/drive-history/scratch/<id>`) as worker `rpool-<host>`, removed
   once everything is published, otherwise kept with the `mount --sync-only`
   command that finishes it.

Paths with local writes not yet uploaded are refused (no race with an unsynced
edit). History changes are based on the revision visible now.

## Mixed versions / limits

- Older RPool: reads restore/rollback events/snapshots as ordinary edits; does
  not see marks (purged entries are just deletions to it); does not defer v7
  GC — in a mixed v7 pool an old PC may collect bytes retention wanted, and a
  listing may show `restorable` for bytes another PC already collected (restore
  then fails clearly; data never substituted).
- v7 `restorable` = bytes in a frontier snapshot, or in a retired snapshot
  whose GC has not started in *this* workspace.
- Rollback/versions of v7 conflicts use the materialization rule
  approximately (no collision renaming); rollback of a conflicted path writes
  normally (conflict copies remain unless they did not exist at T).
- v6 lineages are paths: a deleted-then-recreated path is one lineage; a move
  is followed by bytes identity (heuristic).
- Real cloud eventual consistency (a just-published record missing from a
  listing gets no time until listed) and multi-PC acceptance are not verified.
