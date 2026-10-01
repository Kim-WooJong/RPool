# Read/write Pool drive

`rpool mount` (GUI: the **Drive** page) mounts a pool as an online drive. There
is one drive mode: a virtual drive with automatic pool sync (metadata family
`events-v6`). The file list comes from the pool's synchronized metadata; file
bytes are downloaded and verified on demand; writes land in a local spool and
are uploaded as verified pool archives in the background. Several PCs can mount
the same pool, each with its own workspace. No coordinator PC, metadata server
or shared folder is needed.

An application save is a **local** acknowledgement, not proof that the cloud
copy is complete. For the write path and its durability points see
[MOUNT_WRITE_ROADMAP.md](MOUNT_WRITE_ROADMAP.md); for the native frontends see
[NATIVE_MOUNT_CRYPT_PLAN.md](NATIVE_MOUNT_CRYPT_PLAN.md).

## Quick start

```text
rpool mount --pool mypool --workspace /persistent/rpool/pc-a --mountpoint /mnt/rpool [--pool-worker PC-A]
rpool mount --pool mypool --workspace C:\RPool\pc-a --mountpoint R:
```

- `--workspace` holds local metadata, the write spool and caches. Use a
  persistent folder (never a temporary one), one per PC and pool. Never copy an
  active workspace to another PC.
- `--mountpoint` is an unused drive letter on Windows, or an existing empty
  directory outside the workspace on Unix.
- `--pool-worker` is the PC name shown in conflict copies. Without it RPool
  generates and saves a `pc-…` name for the workspace.
- `--stop-file PATH` requests a clean stop when the file appears (remove it
  before the next start). `--interval-seconds` (default 30) sets the delay
  between background sync passes.
- `--sync-only` synchronizes pending local changes and cloud metadata once
  without mounting. `--capacity-only` refreshes capacity and exclusion warnings
  without mounting or uploading. `--status-file PATH` writes a machine-readable
  capacity snapshot for the GUI.

Requirements: rclone v1.64.0 or newer (v1.74.3 recommended; `rpool doctor`
checks it). WebDAV mounts need WinFsp on Windows or FUSE on Linux; on macOS RPool
uses `rclone nfsmount` with the built-in NFS client (no macFUSE). RPool does not
install system drivers. A running child process alone is not treated as a ready
filesystem; startup times out after 30 seconds.

Removed options (v3 shared roots, v5 bounded shared, v7 peer snapshots, "this
PC only" retention, the full local replica, `--virtual-drive`, `--pool-sync`)
are rejected, and workspaces created by those modes do not open: create a new
workspace.

## Frontends

`--frontend auto` (default) uses RPool's own filesystem frontend where the build
and workspace support it, otherwise rclone mount over RPool's loopback WebDAV
server.

| Frontend | Platform | Notes |
|---|---|---|
| `fuse` | Linux | Native, over the filesystem core; `fsync` and the last `close` are the local durability points. |
| `winfsp` | Windows | Native, same core; needs WinFsp installed (DLL is delay-loaded). Compiled, not yet run on Windows. |
| `dav` | all | rclone mount/VFS (`--vfs-cache-mode full`, 60 s write-back) over the authenticated loopback WebDAV server. The only option on macOS. |

`--native-read-only` mounts a native frontend read-only.

With WebDAV, rclone waits 60 seconds after a file closes before writing its VFS
cache back to RPool, which combines rapid close/reopen writes. Until then the
write exists only in rclone's VFS cache: keep that cache. After a crash, the
next mount imports complete dirty VFS-cache entries as ordinary pending writes;
anything it cannot import safely is kept in `recovered-native-cache/<id>` and
reported, never deleted (Drive page: recovered-cache panel with **Copy path**).
On macOS a native NFS shutdown that exceeds the grace period leaves rclone
running and keeps the mount lease rather than killing the server during kernel
I/O; inspect the OS state before recovering such a workspace.

## Pool sync

Each PC publishes immutable, content-addressed metadata records to every
configured pool destination:

```text
<configured-pool-root>/.rpool-sync/events-v6/<pool-name-hash>/events/<event-hash>.json
```

- The namespace is stable for the same pool name at the same destinations;
  remote order and adding a destination do not change it. **Renaming the pool
  changes the namespace.** Every PC needs the same portable pool/remote
  configuration pointing at the same encrypted data and keys.
- Incoming records are merged by ancestry, not by arrival time or PC clocks.
  Parents publish before children. Downloaded metadata also repairs missing
  replicas.
- Local spool is released only after verified publication to **every**
  metadata destination. An unavailable destination blocks synchronization and
  startup instead of being read as an empty namespace.
- At startup the mount fetches the cloud metadata it needs before opening the
  drive, without first draining uploads. Capacity queries and write-back run in
  one background worker after the mount is ready.
- A new PC reads the newest metadata checkpoints plus the newer records in
  pages. `rpool pool compact` and the mount maintenance loop write checkpoints;
  deleting covered records needs a one-time `--enable-deletion` opt-in and a
  grace period. See [METADATA_COMPACTION_DESIGN.md](METADATA_COMPACTION_DESIGN.md).

### Conflicts

Concurrent edits of the same base keep the common original at its path and
**both** edited heads as `stem_Worker+a-<revision-id>.ext`; the revision suffix
keeps two PCs with the same name apart. Concurrent creation has no original;
mixed delete/edit groups show the delete request. The Drive page shows
**Pool sync · N conflict groups** with original paths, candidates and **Copy
path** buttons (after sync exits: the last snapshot, not a live view). There is
no one-click merge; all variants are preserved.

File lists refresh on synchronization. After a path has been read through
WebDAV, a newer revision fences further opens of that path until remount rather
than mixing range bytes of two versions; open handles keep their revision.

### Incremental uploads

An edit can reuse unchanged remote data of its captured parent revision: plain
archives reuse unchanged shards, Reed-Solomon archives unchanged groups. This
requires equal file size, shard size and coding, and a parent produced by this
workspace's own upload record; other edits (and Resilient placement) use a full
upload. Reused objects are verified, and borrowed objects are never
overwritten. This reduces repeated uploads, not retained revisions.

### Trash, versions and rollback

Deleting or overwriting a file never erases cloud data. `rpool drive trash`,
`rpool drive versions`, `rpool drive rollback` and `rpool drive retention`
(GUI: Files › Library, Storage › Pools › Trash & versions) list deleted files
and earlier revisions and restore them as new revisions — mounted, unmounted or
from the cloud without a workspace. Purge and expiry hide entries; reclaiming
the data is described in [DRIVE_HISTORY_DESIGN.md](DRIVE_HISTORY_DESIGN.md).
Provider drain with `--delete-source` is rejected for drive archives.

### Empty directories

Empty directories are local to the PC that created them; a folder reaches other
PCs once it contains a file.

## Local disk: caches and spool

Independent limits (GiB = 1024³ bytes):

| Setting | CLI | Default | Behavior |
| --- | --- | --- | --- |
| Clean shard cache | `--cache-gib` | 10 | Verified shards, least recently used evicted first. `0` rejects uncached reads. |
| Native OS cache target | `--vfs-cache-gib` | 10 | rclone's VFS cache; open/dirty files may exceed it. |
| Keep disk free | `--cache-min-free-gib` | 2 | rclone free-space target, not a reservation. |
| Pending write spool | `--spool-gib` | 64 | Pending writes incl. partial ones; growth fails safely at the limit. |

These are not one aggregate limit: the caches are separate copies and the spool
adds to both. Upload staging, metadata, recovery exports and other applications
need more space. Reading evicted content downloads and verifies it again, so it
is not available offline. A working set larger than the shard budget is refused
with an error that names the budget.

Settings apply to the next mount. In the GUI they are saved per pool on this PC
together with the workspace, mountpoint and PC name; switching pools on the
Drive page never stops another mounted pool.

Keep the whole workspace (`.rpool`, spool, caches) on reliable local storage. It
contains plaintext; use OS disk encryption where needed.

## Recovery and maintenance

Restart the **same** workspace and mountpoint to replay pending work. Do not
delete `vfs-cache`, `spool`, `dav-identity.json` or the mount identity files. A
corrupt primary namespace fails closed instead of silently reverting.

- `--recover-spool` (Drive page: **Export recoverable spool**) copies sealed or
  partial spool files to `recovered-writes/` with receipts naming the original
  paths. It does not mount, upload or rewrite metadata. Writes that exist only
  in rclone's VFS cache are recovered by the next mount, not by this export.
- `--cleanup-cache` (**Trim clean cache**) trims verified clean shards and
  committed spool without readers. Pending, partial and unknown writes stay.
- A lease in `.rpool/mount-process.json` blocks a second mount of the same
  workspace. If an error names it, first confirm in the OS that no rclone
  process or mount uses the workspace; only then remove **that file alone**.
  Keep `mount-identity.json`, the spool and the VFS cache.
- Unmount cancels remote operations and stops the mount without a full sync;
  pending spool and uncertain uploads are kept and reported, never shown as
  uploaded. Use `--sync-only` to finish replication without mounting.

A pool, a workspace (also nested) or a mountpoint is used by one mount at a
time; the GUI and CLI refuse a second one and the GUI offers a free drive
letter. `rpool mount monitor` lists running mounts and their traffic.

## Changing the pool

- **Layout changes** (shard size, K/M, placement, worker/retry): the next mount
  uses the new policy. Uploads already pending finish with their recorded
  layout first; the mount reports the deferral.
- **Membership changes** (accounts added/removed): unmount, then **Apply pool
  changes** (`rpool mount --pool P --workspace W --apply-pool-changes`). It
  verifies current files, visible conflicts and sealed pending writes into a
  fresh metadata generation, keeps the pool name and workspace path, and keeps
  the original workspace data in a sibling backup. It does not mount; mount the
  same workspace afterwards. Other PCs keep the old generation until they apply
  too. `--recovery-reprocess-plan PATH` supplies completed Reprocess results for
  files whose old account is gone. Nothing in the cloud is deleted.
- **Moving the stored data** to the pool's new policy for every PC: `rpool pool
  migrate plan|run|adopt` migrates archives and the drive and publishes a new
  drive generation that every PC opens. See
  [POOL_MIGRATION_DESIGN.md](POOL_MIGRATION_DESIGN.md).

## Recover after losing an account

`--account-recovery-from` copies locally known recoverable files into a **new,
differently named** pool and workspace, leaving the source untouched. It does
not mount.

1. Stop the original mount. Keep its workspace, rclone remotes and cloud data.
2. In Pools, save the remaining accounts as a new pool. Select it with a new
   empty workspace.
3. Run recovery from the original workspace. Name failed accounts by rclone
   remote alias (`--recovery-skip-remote failed-crypt`, repeatable). Optionally
   reuse a completed Reprocess plan (`--recovery-reprocess-plan`); originals
   are matched by manifest fingerprint and completion receipts, never by name.
4. Read `account-recovery.json` in the destination, then mount the destination
   normally. Resume an incomplete recovery with the same source and destination.

```powershell
rpool mount --pool recovered-pool --workspace C:\RPoolRecovered --account-recovery-from C:\RPoolOriginal --recovery-skip-remote failed-crypt
rpool mount --pool recovered-pool --workspace C:\RPoolRecovered --mountpoint R:
```

The source is its locally known view, not a fresh cloud listing. Files never
synchronized to it, earlier versions, partial writes and dirty VFS cache are
not declared recovered; an unavailable file is a reported failure, never an
empty file. No account is deleted and an authentication error is never taken as
proof that data is absent. Recovery costs transfer, destination capacity and
temporary local space.

## Importing data

- `--import-from REMOTE:PATH [--import-to FOLDER] [--import-batch-gib 4]
  [--import-conflict skip|rename]` copies a tree stored with plain rclone into
  the drive and uploads it as pool shards. Unmount first; it does not mount.
  The source is only read. Uploads run every batch, so local disk holds one
  batch. Existing drive files are skipped (or imported as `name (imported
  N).ext`); names the drive cannot store are listed. A journal in
  `.rpool/imports/` makes reruns resume without duplicates. Modification times
  are not kept. GUI: Drive page, Import.
- `--manifest FILE` (repeatable) adds archives made with `rpool put` to the
  drive at the next mount or sync; pool membership is never inferred. Remove
  the arguments after importing.

## Capacity and placement

See [POOL_CAPACITY.md](POOL_CAPACITY.md). Mount write-back queries real account
quotas; missing, contradictory or stale results are reported separately, and
zero free means full, not unknown. The drive reports known namespace usage and
**zero additional free space** while capacity is missing, expired or being
refreshed; Windows Explorer's "1 PB free" is rclone's unknown-quota fallback,
not the pool's capacity. Values are logical estimates after coding, placement
and pending reservations, not a sum of provider quotas. Usage counts live files
plus sealed pending changes, excluding parity, history and caches.

Declare accounts that share a quota or fail together (account identities
editor in Storage › Pools, or `--capacity-domain backing=account` and
`--failure-domain backing=group`). These are user declarations, not proven
identities. Without declarations, unresolved accounts conservatively share the
smallest reported budget; once some are declared, undeclared accounts are
excluded until mapped. Capacity uses the same allocator as uploads
(round-robin, free-ratio, resilient, capacity-first); the simulation is capped
at 262,144 shards and then reported as a lower bound.

## Limits

- Pool sync is eventual synchronization, not distributed locking or
  cross-PC open-handle coherence.
- Payload history accumulates until reclaimed (see
  [DRIVE_HISTORY_DESIGN.md](DRIVE_HISTORY_DESIGN.md)); quota is not reserved
  across PCs.
- WinFsp has not been run on Windows; the macOS mount uses WebDAV over NFS.
  Real multi-PC operation is covered by Docker FUSE end-to-end scripts
  (`scripts/linux-docker/`) and synthetic tests, not by long-running cloud use.

References: [rclone mount](https://rclone.org/commands/rclone_mount/),
[VFS file caching](https://rclone.org/commands/rclone_mount/#vfs-file-caching).
