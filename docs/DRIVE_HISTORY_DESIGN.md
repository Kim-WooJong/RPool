# Drive history: trash, file versions, rollback

Status: implemented (backend + CLI, `src/drive_history/`). GUI reads the JSON
contract in `src/drive_history/model.rs` (or calls `drive_history::api`).

## What the format already keeps

The drive (v6 pool sync, the only drive mode) keeps:

- **History records**: one immutable event per change (`Event {path, parents,
  content}`; `content: None` = delete). Never deleted; gated metadata
  compaction moves them into checkpoints, losslessly.
- **Bytes**: every revision's manifest stays readable until the guarded drive
  cleanup deletes archives nothing keeps (see "Retention and data lifetime").
  Incremental uploads may share shard objects between revisions.
- **Time**: no timestamp in `Event`.

So the history itself (who changed what, ancestry, deletions) is always
complete; what can expire is **bytes** (drive cleanup).

## Times (format decision)

Adding a time field to `Event` is **not** compatible: ids are
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

`graph::History` holds revisions with lineage (the event path), parents,
content (hash, size, restorable), author (worker), time, and a projection
"what is visible at time T" (the drive's own `peer_projection::project` over
the events up to T). Trash, versions and rollback are
pure functions over this graph (unit tested with fake graphs).

- **Trash**: lineages whose every head is a deletion. Entry id = deletion
  revision; bytes = nearest earlier content. Hidden: purged, expired, and
  deletions whose bytes are visible elsewhere (moves, restored copies,
  identical copies); identical deleted bytes are listed once (newest).
  `dir:/path` aggregates a gone folder holding ≥ 2 trashed files.
- **Versions**: every revision of the lineage incl. deletions and conflict
  branches, newest first (effective time, then causal depth). It also follows
  a move (new path whose first revision has the deleted old path's bytes).
- **Rollback to T** (scope = folder or `/`): diff of visible files at T and
  now: Revert (other bytes now), Undelete (gone now), Remove (created after
  T → trash). Paths whose bytes are no longer available are `skipped`.

Every operation **publishes new revisions** (normal events older RPool reads
as ordinary edits): restore references the old content again (one new event
descending from what is at the path now, exactly like a move; no bytes move).
Remove = ordinary delete. Rollback is therefore itself
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
- Expiry and purge only hide entries and make data *eligible*
  (`trash purge` reports `eligible_bytes`). Data is deleted only by the
  guarded **drive cleanup** below.

### Drive cleanup (physical deletion)

`rpool drive cleanup --pool P [--workspace W] [--confirm] [--force] [--json]`
(`src/drive_history/cleanup/`), `--cancel` drops pending marks. The GUI
(Storage › Pools › "Trash & versions") shows "Reclaimable / Waiting until"
and runs "Check now" (preview) / "Clean up now" (`--confirm`, a second
confirmation adds `--force` when the guard would stop it).

- **Unit**: an *archive* = a top-level `virtual-*` folder on a pool account
  (a file version's own archive with its manifest replicas and shards, or an
  incremental upload's group part `virtual-<id>-g<n>`). Every revision's
  manifest names the folders of its shards (an incremental version also its
  base's folders).
- **Kept**: every folder named by a revision that is current, in a trash
  entry neither expired nor purged, or a kept version (`retention::protected`;
  a missing record time never expires), in **every** drive generation
  (events incl. checkpointed ones via `metadata_pool::read_v6`); by this PC's
  open drive (its unpublished events, open files, parents of pending
  uploads) when the cleanup runs in a mount or `--workspace`; and every
  folder or object named by the migration cleanup's reference sources
  (`migration/retire/live_refs`: local inventory, non-drive manifest
  replicas, migrations in progress) or by a `virtual-*` manifest no event
  names (an upload in progress). Shared objects keep the whole folder while
  any referrer is kept. Only folders named exclusively by other revisions
  are candidates (a folder never named by an event is never touched).
- **Two steps**: a confirmed run publishes a content-addressed **mark**
  record (`{format:1, kind:"cleanup", step:"mark", mark, archives, grace_seconds,
  worker, unix}`) in the same `history/events/` directory as purge marks on
  every replica of the newest generation (older RPool never lists it; purge
  readers ignore the kind). A later run past the grace (pool setting,
  default 7 days) re-checks every reference **fresh**, journals `deleting`,
  observes again, deletes what is still unreferenced (manifest replicas
  first, then shards) and journals `deleted` in batches. Data referenced
  again during the grace is released (`cancel`); one referenced again during
  the deletion re-check is released before anything is deleted.
  `deleting` is the point of no return: an interrupted deletion resumes on
  the next run of any PC and a user `--cancel` cannot stop it. Revisions in
  `deleting`/`deleted` archives are no longer `restorable` on any PC.
- **Fail closed**: any unreadable reference source (generation metadata, a
  manifest, the inventory, a migration journal, an account listing) postpones
  the whole run (`mode: postponed`; nothing marked or deleted). Missing record
  times and unreadable purge marks only keep more.
- **Mass-delete guard**: a run refuses to delete more than 50 % of the
  drive's stored objects or bytes, or more than 10,000 objects, unless
  `--force` (the preview reports `guard` so the GUI can ask twice).
- **Automatic**: each mount's maintenance loop runs a confirmed, never-forced
  pass 30 minutes after start and then daily (marks new candidates, deletes
  due data); errors and postponements are logged only. Per pool:
  `rpool drive retention set --auto-cleanup false|true --cleanup-grace-days N`
  (`pools.json` → `drive_cleanup`, default on / 7 days, at least 1 day).
- **Limits**: local unpublished revisions of *other* PCs (an offline PC's
  pending restore of an old version, or its stale view) cannot be seen. The
  grace period is what protects them: choose it longer than any PC stays
  offline. A restore published by another PC between this run's last
  re-check and its deletion (a seconds-long window) is not seen either.
  `keep_versions` trimming is a count, not an expiry, so it applies even to
  versions with unknown times. An older RPool does not see marks and may
  still offer a deleted version for restore (the read then fails; data is
  never substituted).

Purge (`trash purge --id … | --expired`, `trash empty --confirm`) publishes an
immutable, content-addressed **mark** `{format:1, kind:"purge", ids, worker,
unix}` under `<metadata root>/history/events/<blake3>.json` on every replica of
the current generation. Older RPool never lists `history/`. Marks hide the
entries on every PC and end their retention protection; they delete nothing.

## Where it runs

1. **Mounted** (this PC's mount registry lists a mount of the pool or of
   `--workspace`): the CLI writes `<workspace>/drive-history/requests/<id>.json`
   (atomic rename); the mount's maintenance loop polls every 2 s, renames it to
   `.working`, runs it on its own `VirtualDrive`, answers in `responses/`. Only
   the mount may touch the locked workspace, and its namespace/pins/sync see
   the change immediately. An older mount never answers: the client times out
   and withdraws its unclaimed request (nothing runs later by surprise).
2. **Unmounted `--workspace`**: opened directly (lock, like a mount); changes
   publish as that workspace's worker.
3. **No workspace**: listing/preview read the cloud read-only like
   `pool browse` (`metadata_pool::read_v6` incl. checkpoints); purge publishes the mark;
   restore/rollback run in a scratch workspace on the newest generation
   (`<config>/drive-history/scratch/<id>`) as worker `rpool-<host>`, removed
   once everything is published, otherwise kept with the `mount --sync-only`
   command that finishes it.

Paths with local writes not yet uploaded are refused (no race with an unsynced
edit). History changes are based on the revision visible now.

## Mixed versions / limits

- Older RPool: reads restore/rollback events as ordinary edits; does not see
  marks (purged entries are just deletions to it).
- Rollback of a conflicted path writes normally (conflict copies remain unless
  they did not exist at T).
- Lineages are paths: a deleted-then-recreated path is one lineage; a move
  is followed by bytes identity (heuristic).
- Real cloud eventual consistency (a just-published record missing from a
  listing gets no time until listed) and multi-PC acceptance are not verified.
