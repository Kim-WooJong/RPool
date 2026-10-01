# Pool metadata checkpoints and compaction (v6 events)

Status: implemented (format 1). Applies to the drive's pool-sync metadata, the
v6 namespace events (`.rpool-sync/events-v6/<scope>`), including their
`epochs/<epoch>/` generations.

## Problem

Every v6 event is one immutable, content-addressed object. A fresh
bootstrap (new PC, lost workspace) used to read all of them and failed at
10,000 unseen records or 64 MiB per replica ("peer compaction is not yet
implemented"). A long-used drive therefore became unopenable on a new PC.

## Objects (per family root `R`, on every replica)

| path | content |
|---|---|
| `R/checkpoints/chunks/<id>.json` | `Chunk {format:1, family, records: {kind: {record id: exact record JSON}}}` (≤ 8 MiB) |
| `R/checkpoints/heads/<id>.json`  | `Head {format:1, family, created_unix, chunks:[chunk ids], records, bytes}` |
| `R/checkpoints/marks/<id>.json`  | `Mark {format:1, family, checkpoint:<head id>, marked_unix, records:{kind:[ids]}}` |
| `R/events/<gate>.json` | compaction gate record (below) |

All objects are content addressed (`id = blake3(bytes)`), verified on every read,
written with readback and never overwritten — exactly like existing records. They
live inside the pool's encrypted metadata roots (rclone crypt or RPool native
crypt), so they are as confidential as the records they contain. There is no
signing key in RPool; integrity comes from content addressing: a checkpoint can
only *repeat* records whose ids it names, it cannot invent or alter one, because
every embedded record is re-hashed against its id and re-validated by the normal
model (`Event::validate`, `reduce`).

A checkpoint is **lossless**: it embeds the exact bytes of the records it covers.
Heads are incremental: a new head lists every chunk of every valid existing head
plus new chunk(s) holding only the records not yet covered. So any new head
supersedes all older ones, chunks are shared, and concurrent writers simply
produce two heads whose union is still correct (the next pass merges them).

## Bootstrap / pull

1. List the record directories of every replica (an unreadable replica is an
   error, never an empty namespace — unchanged rule).
2. If this is a bootstrap (no known records) **or** the gate record is listed,
   list `heads/` on every replica, read every valid head, and read each chunk not
   yet ingested (local cache `metadata-checkpoints-<family>.json` in the
   workspace). Records from chunks are verified and ingested like downloaded
   records and count as published (they are durable in the checkpoint).
3. Read the remaining unseen records individually (the tail), in pages of at most
   10,000 records / 64 MiB; parsed records accumulate, raw bytes are dropped per
   page. There is no longer a hard 10,000 / 64 MiB failure; a far larger safety
   ceiling (1,000,000 records / 16 GiB per pull) remains with an actionable message.

Cost: O(heads + new chunks + tail) objects instead of O(all records).

Invalid heads/chunks (bad hash, unparsable, missing chunk) are **ignored** while the
gate is absent: nothing was ever deleted, so the tail still contains every record.
With the gate present an unusable head fails closed ("retain workspace"), because
records it covers may already be deleted.

## Compaction (`rpool pool compact`, and the mount maintenance loop)

1. **Checkpoint**: when uncovered records ≥ `checkpoint_after_records` (2,000) or
   ≥ `checkpoint_after_mib` (16 MiB) — or on a manual run with any uncovered
   record — write new chunk(s), then the new head, to every replica.
2. **Mark** (gate required): for the newest head `H` (its chunks ⊇ every other
   valid head's) present with all chunks on *every* configured replica, publish a
   mark listing the covered record ids still present. Marks are immutable.
3. **Delete** (gate required), only when a mark is older than `grace_days` (14)
   **and** its head and all chunks are still listed on every replica (sizes equal)
   and every marked id is covered by that head's verified chunks. Then the marked
   records are removed from every replica (idempotent), heads whose chunks ⊆ `H`
   are removed, and finally the mark. A stale mark (its head gone) older than the
   grace period is removed.

Deletion therefore needs: a newer checkpoint on all replicas for ≥ grace period,
two separate passes (mark, later delete), and the gate. "All replicas" is stricter
than a majority, matching the existing rule that reads require every replica.
Chunks are never deleted in format 1 (heads only grow), so metadata stays bounded
in *object count* (heads + chunks + a recent tail), while total bytes still grow
with history (see Limits).

The mount runs compaction at most every `interval_minutes` (60) after a
successful sync, under the drive's sync gate; failures are logged and reported
as a monitoring alert, never fail the mount. Thresholds live in
`<config dir>/metadata-compaction.json` (all optional):
`{"auto": true, "checkpoint_after_records": 2000, "checkpoint_after_mib": 16,
"grace_days": 14, "interval_minutes": 60}`.

## Mixed versions — compatibility rule

* Checkpoint heads/chunks/marks live in new directories that older RPool never
  lists, so writing checkpoints is invisible to older versions. **Without the
  gate nothing is ever deleted**, so older versions keep working exactly as before
  (including their old 10,000 / 64 MiB bootstrap limit).
* Deletion is gated by an explicit, one-time opt-in:
  `rpool pool compact <NAME> --enable-deletion` publishes the **gate record**, an
  `Event` with `version: 2` in `events/`.
  Newer RPool recognises the fixed gate id and skips it. Older RPool parses it and
  stops with "unsupported namespace event version" on its next pull or bootstrap — it is broken *loudly*, never
  silently shown an incomplete drive. Enable deletion only after every PC runs a
  version with checkpoints (this one or later).
* Future formats bump `format`; readers ignore heads of unknown format (and fail
  closed if the gate is present), writers never merge chunks of unknown format.

## Early warning

`rpool doctor` lists each pool's metadata (one recursive listing per replica) and
warns when the record count approaches the old bootstrap limit (≥ 8,000 records or
≥ 51 MiB: older RPool cannot bootstrap it), or when there are ≥ 2,000 records and
no checkpoint newer than 30 days. The mount's compaction pass sets the monitoring
alert `metadata_growing` (Monitoring page) when uncovered records exceed the same
warning level or compaction fails.

## Limits / future work

* Lossless format: total checkpoint bytes still grow linearly with history; the
  local workspace keeps every record (as before). Semantic pruning (dropping
  superseded events) would change conflict projection (common ancestors) and
  the history that trash/versions/rollback read, and is deliberately not done.
* Chunk consolidation (merging many small chunks) is not implemented; each
  checkpoint adds ≥ 2,000 records per chunk by default, so chunk count stays low.
* A single record whose JSON-escaped form exceeds a chunk (≈ 6 MiB) is never
  checkpointed and therefore never deleted.
* Listings of one directory are still bounded by the 8 MiB rclone output cap per
  hash-prefix page (≈ 1.3 M records); with deletion enabled this is not reached.
