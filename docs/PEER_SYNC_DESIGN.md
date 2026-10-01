# Coordinator-free shared drive (pool sync v6)

Status (2026-10-01): pool sync v6 is the only drive mode (`rpool mount`). The
earlier designated-coordinator protocol (v5), shared roots (v3) and the v7
private-snapshot protocol with peer payload GC were removed with their code;
their workspaces are refused at open. Usage and limits: [MOUNT.md](MOUNT.md).

## Requirements

- No designated PC, no metadata service, no provider-specific conditional API
  and no global mutable checkpoint written by competing PCs. RPool keeps its
  protocol metadata inside the pool's existing encrypted destinations.
- Different logical paths commit independently.
- A sequential accepted edit replaces the current version.
- Concurrent edits of one base preserve that base at the original path and
  expose every edited branch as `stem_worker+a-<unique-operation-id>.ext`.
- Worker display names need not be unique; immutable operation/device identity
  disambiguates them. Portable-name and case-collision validation stays.
- Offline PCs pull the current state; their pending local edits keep the
  revision they were actually based on.

## Protocol

Each PC publishes immutable, content-addressed event records, replicated to
every configured pool destination:

```text
<configured-pool-root>/.rpool-sync/events-v6/<pool-name-hash>/events/<event-hash>.json
```

- An event (`src/mount/shared_model.rs`) names a path, the worker and device
  identity, its parent events and either the new content (hash, size, archive
  manifest) or none (a deletion). A rename is the new path plus a deletion of
  the old one. Payloads are uploaded and verified before the event is
  published.
- Clients merge every event they observe by ancestry (a DAG), never by arrival
  order or clocks, and compute the same projection: current files plus
  structured conflict groups.
- Parents publish before children. A record counts as published only when it is
  verified on every metadata destination; spool is released after that. A
  failed or partial publication retries the same immutable ID. Downloaded
  records repair missing replicas.
- An unavailable destination or failed listing blocks sync and startup; it is
  never read as an empty or shrunken namespace.
- Edits may reuse unchanged shards/groups of their own parent revision
  (incremental uploads); borrowed objects are never overwritten.

There is no compare-and-swap anywhere: blind overwrite plus readback is not
CAS, so every record is write-once and every decision is derived from the set
of immutable records.

## Conflict projection

Structured groups, never filename parsing:

- logical path and stable group identity;
- original/base identity and availability;
- each worker/device candidate (path and revision) or deletion request.

The projection uses the unique maximal common content ancestor, not the oldest
ancestor or a hash-elected head. Concurrent creation has no original. Multiple
maximal common ancestors are shown as separate original candidates.
Multi-generation branches keep the common fork original and each maximal head.
The Drive page lists groups (**Pool sync · N conflict groups**) with **Copy
path**; there is no one-click resolution yet, and all variants are preserved.

## Growth and its bounds

- **Metadata:** the event set grows with every edit. Checkpoints written by any
  PC cover old records so a new PC opens the drive from a few objects plus the
  newer tail; deleting covered records is opt-in. See
  [METADATA_COMPACTION_DESIGN.md](METADATA_COMPACTION_DESIGN.md).
- **Payloads:** every revision's archive stays referenced by its event, so
  payload history accumulates. Trash, versions and rollback build on that
  history; reclaiming data of purged or expired entries is described in
  [DRIVE_HISTORY_DESIGN.md](DRIVE_HISTORY_DESIGN.md).

Any reclamation must keep the rule that obsolescence needs positive evidence
(a durable successor, a purge mark seen on every replica, a grace period),
never a missing listing entry or a timeout, and must not let a late publisher
reference an object that another PC already deleted.

## Validation

Covered by synthetic tests (local storage fixtures, fake rclone, crash and
randomized-trace matrices in the filesystem core) and by Docker FUSE end-to-end
scripts with real rclone crypt over local directories
(`scripts/linux-docker/pool-sync-e2e.sh`, `cache-recovery-e2e.sh`,
`rclone-import-e2e.sh`, `pool-browse-epoch-e2e.sh`):

- two PCs, disjoint paths: independent progress, no lost updates;
- one base, two edits: original plus both candidate bytes, stable names;
- delete/edit, concurrent creation, atomic save (rename over), sequential edits
  without conflict copies;
- crash after upload and at publication boundaries;
- no plaintext names or content in the remotes.

Not validated: long-running real-provider multi-PC use, eventual-consistency
behaviour of specific providers, and WinFsp on Windows.
