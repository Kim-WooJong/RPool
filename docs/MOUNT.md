# Read/write Pool drive

Two modes are available. **Replica** (default) keeps a full local copy. The new
**Virtual cloud drive** is opt-in: it lists the shared metadata namespace without
restoring all files, then downloads/verifies only intersecting shards on reads.
Both modes keep writes on local disk before asynchronous verified cloud publication.
An application save is **not** a completed-cloud-replication acknowledgment.

## Virtual drive (new workspace only)

Select **Virtual cloud drive** in Mount drive, a NEW empty persistent workspace,
Pool, mountpoint, and optionally shared root + worker name. All participating PCs
must use virtual mode and the same shared root; its events live under
`shared-root/virtual-v3`, isolated from older replica catalogs. Existing archives
are imported explicitly from manifests; listing imports does not download content.
Never point this mode at an existing replica or copy/share an active workspace.

The authenticated loopback DAV bridge mounts through rclone, requiring WinFsp on
Windows or a mount-capable FUSE installation on Linux/macOS. Its private persisted
endpoint/token must remain unchanged alongside the VFS cache after interruption.
An occupied saved port or live/uncertain old mount lease fails closed.

- Range reads verify whole requested shards (the archive format has no subshard
  hashes). Missing/unavailable data uses the existing parallel RS recovery scheduler
  for only the affected coding group. Clean shard cache is bounded; dirty spool is
  never evicted. Uncached content needs network access.
- PUT completion at the bridge flushes content, recovery receipt and namespace.
  Native application writes can still be in rclone's VFS cache; keep that cache too.
- Shared changes are fetched while mounted. Previously unserved paths update live.
  Served paths retain an immutable mount-session revision; newer remote versions
  appear as `conflict-incoming-ID` copies until remount. This avoids combining bytes
  from different versions across generic DAV range requests. Local acknowledged
  saves/deletes/renames update their visible paths; already-open DAV read handles
  retain their selected revision. DAV mtime is a synthetic revision discriminator,
  not the original source modification time. **No distributed locking or transparent native
  open-handle coherence guarantee.** Generic writes use conservative observed bases,
  so some sequential edits can produce extra conflict copies rather than overwrite.
- File and directory moves checkpoint their local namespace changes together.
  Empty directories remain local-only. Metadata-only deletion is not remote erasure.
- Usage counts known live shared file contents plus sealed pending changes,
  **excluding parity and cache copies**. A separate committed logical counter is
  shown. Unimported archives and writes still in VFS cache are not included.
- Virtual mode exports this logical usage and placement-aware ceiling via standard
  DAV quota. Real HTTP quota tests pass; Explorer/WinFsp and live rclone mount
  behavior have **not** been validated on a real multi-PC deployment.

### Recovery and safe cleanup

Restart the SAME workspace/mountpoint to replay durable pending work. Do not delete
`vfs-cache`, `spool`, `dav-identity.json`, or mount identity files. If the primary
namespace is corrupt, RPool fails closed instead of silently reverting to a backup
that might omit acknowledged writes.

**Export recoverable spool (unmounted)** / `--virtual-drive --recover-spool` copies
sealed or partial spool files to `recovered-writes`, with receipts preserving the
original paths. It does not repair/replace a corrupt checkpoint or upload anything.
Partial files are explicitly labelled and need inspection. Recovery cannot export
writes that exist only in rclone's cache; recover those through the original mount.

**Trim clean cache** / `--virtual-drive --cleanup-cache` trims verified clean
shards and reclaims verified, committed local spool that has no readers. Pending,
partial, corrupt and unrecognized writes remain available for recovery. Normal
successful sync also reclaims committed spool, but shared writes wait for verified
metadata publication. Read handles and in-progress writers hold leases; cleanup
advances the recovery checkpoint before removing any file.

Virtual DAV write growth (including local moves) is bounded by `--spool-gib`
(default **64 GiB**). Existing bytes are preserved when the limit rejects a write.
This is not a reservation of free disk: VFS cache, staging snapshots, recovery
exports and other applications need additional space. `--cache-gib` remains the
separate clean-shard cache limit (default 10 GiB). CLI status JSON exposes
`spool_bytes`, `spool_limit_bytes` and `pending_writes`.

### Explicit cloud retention — unshared virtual workspaces only

Automatic cloud deletion is **not enabled**. Start with a read-only preview:

```text
rpool mount --virtual-drive --pool mypool --workspace /absolute/workspace --retention-report --keep-previous 3
```

For an **unmounted, drained, exclusively owned** virtual workspace, an operator
can explicitly apply that policy:

```text
rpool mount --virtual-drive --pool mypool --workspace /absolute/workspace --apply-retention --keep-previous 3 --exclusive-archive-ownership
```

The ownership acknowledgment means **no other workspace, PC, exported manifest or
external reader depends on these archives**. Never use it merely because a drive
is currently disconnected. Shared roots are rejected. Pending writes, active
readers, detached VFS cache and unknown/partial spool block the operation; replay
or recover them rather than deleting cache to force maintenance.

- All live versions and unresolved conflict versions are protected, plus the
  requested number of previous tracked versions per original path, in durable
  local upload order. `0` keeps live/conflict versions only.
- Ownership is recorded for successful uploads made by this version. Imported,
  legacy/untracked archives and failed-upload leftovers are **not** automatically
  adopted or deleted. An imported alias of an archive protects its entire identity.
  Thus the first preview may show no reclaimable data despite old cloud usage.
- Retained versions are verified before deletion. Only exact recorded shard and
  manifest objects are removed—never a remote prefix/directory purge.
- A checksummed journal makes interruptions resumable. Ordinary mounting/sync is
  blocked until the same apply command completes; changed `--keep-previous` values
  do not replace an in-progress plan. Do not remove the journal.
- Before deletion, local namespace version **4** fences older binaries. Do not
  downgrade this workspace. A completed sole-writer checkpoint removes obsolete
  local ancestry while keeping every visible/conflict file. Kept historical
  manifests remain in `owned-archives.json`.
- Provider trash, provider-native versions and encryption overhead mean estimated
  logical object bytes are **not proof of immediately recovered provider quota**.
  RPool does not empty provider trash.

**Remaining shared-mode limit:** bounded automatic cloud retention needs an
explicit epoch/checkpoint and offline-writer fencing protocol. Existing shared
roots retain cloud history; this update does not make their storage growth bounded.
Do not manually delete causal events or old archive objects to work around quota.

## Replica GUI

1. Create/select a Pool under Storage → Pools.
2. Open Storage → Mount drive. Select the Pool and a durable local workspace
   (empty folder on first use; choose the same folder on subsequent runs).
3. Optionally select explicit existing manifests to restore. Existing archive Pool
   membership cannot be inferred, so archives are never imported automatically.
4. Choose an unused drive letter such as `R:` on Windows, or an existing empty
   absolute mount directory **outside the entire workspace** on Unix.
5. Mount read/write. Add/edit/rename/delete files through the drive. Close open files,
   then use Unmount and inspect the final scan. Sync once archives already-materialized
   local files without mounting; it cannot drain a detached VFS cache.

Install rclone plus **WinFsp** on Windows, or a compatible FUSE/mount-capable rclone
installation on macOS/Linux. RPool does not install system drivers. A running child
process alone is not considered a ready filesystem. Startup times out after 30 seconds.

## Quota-aware placement and capacity

Mount writeback queries actual backend quotas instead of rejecting provider types
with a fixed allowlist. Missing total/free, contradictory results, unsupported
wrapper resolution and transient query failures have separate diagnostics. Zero
free space means full, not unknown. Exclusion affects new placement only; old
manifests remain readable.

The **Account capacity / outage identities** editor accepts one line per concrete
backing remote (not its crypt wrapper):

```text
backing-a account-a provider-a
backing-b account-b provider-b
```

These are non-secret user declarations, not automatically proven account identities.
Use the SAME quota-group ID for remotes sharing an account/quota. Use the SAME outage
ID for accounts that fail together. Independent budgets are summed; alias budgets
are counted once. Without declarations, all unresolved accounts share the smallest
reported budget conservatively. When some accounts are declared, unresolved accounts
are excluded until mapped, because they might alias one of the declared accounts.
CLI equivalents: `--capacity-domain backing-a=account-a` and
`--failure-domain backing-a=provider-a`.

Capacity uses the SAME round-robin/free-ratio/resilient allocation logic as fresh
uploads, debiting actual account budgets. Resilient now requires declared outage
groups and limits each group's shard concentration to M. It skips targets without
enough account quota, charges shared account budgets once and places larger shards
first within each group. Both upload and estimation use the same allocator;
Resilient metadata replicas go only to targets actually used by that archive.
This is a deterministic feasible greedy policy, not an optimal packing solver.
Saved uploads validate
remaining shards against their saved destinations, crediting only reverified data;
parity remains fully charged. Existing Pool policies are not silently changed.

The panel shows logical used bytes, additional full-group capacity, their sum and
snapshot age. Simulation is bounded to 65,536 physical shards; if capped, the panel
explicitly marks a verified **lower bound, not a maximum**. Small nonempty files need
`L + ceil(ceil(L / shard_size) / K) * M * shard_size` physical bytes. Empty mount files
are uncoded. Metadata/encryption overhead and external writers can reduce usable
space; these are estimates, not reservations or nominal provider capacities.

Replica mode counts local files and OS space remains the local staging disk.
Virtual mode counts its known shared namespace and provides quota through DAV.
Historical archives consume provider quota even though they are not logical live
file usage. No mode infers a complete cloud inventory from a Pool name.

### Move active data away from excluded storage

The panel warns when locally known manifests reference excluded storage. After
unmounting and draining the original VFS cache, **Migrate active archives — retain
originals** restores each affected active archive, uploads it to currently eligible
targets, fully verifies it and atomically switches the workspace reference. Pending
local edits are not substituted for the archived revision and are not overwritten.
The operation holds the workspace lock. Restart reuses a durable verified-copy
receipt only after checking identity, eligibility and remote content again.

Original shards, old manifests and immutable shared history are **not deleted**;
other PCs and historical revisions can still reference them. This action does not
free old storage and does not migrate every historical/cloud object. Warnings count
locally known history as well as active files; unrelated archives elsewhere cannot
be discovered from a Pool name. Manifest-only changes publish shared successor events
so peers receive the new location without losing older history. Shared namespace-root
metadata is separate from shard eligibility and is not relocated by this action.
Partial batch failure preserves completed reference switches; the next shared sync
publishes any pending revisions.

Interrupted uploads reuse verified completed manifests and finish remote metadata
publication. Partially uploaded source-matching data earns quota credit only after
full remote verification; parity is conservatively charged again. Changed eligible
sets use separate upload journals and may leave additional retained copies.

CLI maintenance (no mountpoint required):

```text
rpool mount --pool mypool --workspace /absolute/workspace --capacity-only
rpool mount --pool mypool --workspace /absolute/workspace --migrate-excluded
```

Shared workspaces still require the same `--shared-root` and `--worker-name`.
`--status-file PATH` writes a GUI-readable snapshot atomically; its parent must exist.

## CLI (PowerShell example)

```powershell
rpool mount --pool mypool --workspace C:\RPoolWorkspace --mountpoint R: --interval-seconds 30 --stop-file C:\RPoolControl\stop.request
# Optional imports: append --manifest C:\Manifests\example.rpool.json
# To request shutdown from a second terminal (parent folder must exist):
New-Item C:\RPoolControl\stop.request -ItemType File
# Remove the old stop request before starting again.
rpool mount --pool mypool --workspace C:\RPoolWorkspace --sync-only
```

Keep the service running while using the drive. Force termination is a recovery path,
not a flush guarantee. Restart using the same workspace, source, mountpoint and VFS
cache settings so rclone can recover cached writes. Never delete the cache to clear an
error. If a surviving mount is detected, stop that mount before reopening the workspace.

The first mount saves its source/cache/target identity. Reuse that identity on restart;
moving the workspace or selecting another drive letter is intentionally rejected while
pending cache may exist. An uncertain launch or live/reused PID blocks restart rather
than killing an unrelated process. If the error names `.rpool/mount-process.json`,
first confirm in the operating system that no rclone process or filesystem mount uses
this workspace. Only then may that **lease file alone** be removed for recovery; keep
`mount-identity.json`, all local files and the VFS cache. A clean stop clears the lease
automatically. Ctrl+C/forced termination is not a verified graceful shutdown path.

## Shared workspaces (multiple computers)

Set **Shared encrypted folder** and **Worker name** on the Mount drive screen,
or use both CLI options:

```powershell
rpool mount --pool mypool --workspace C:\RPoolAlice --mountpoint R: --shared-root crypt:teamspace --worker-name Alice
rpool mount --pool mypool --workspace C:\RPoolBob --sync-only --shared-root crypt:teamspace --worker-name Bob
```

Every computer uses its **own local workspace/cache**, but the same actual shared
encrypted directory. Each computer must have working rclone crypt configuration,
keys and remote aliases for the archives being shared. A matching Pool name alone
does not establish a shared folder. Keep worker names recognizable; equal names
are supported through distinct device/revision identities. Never copy an active
workspace or synchronize its `.rpool`/VFS cache between computers.

Shared mode publishes immutable, verified, content-addressed revision records.
The records contain file paths, worker/device identity, parent revisions and full
archive manifests; they are encrypted using the normal crypt-only write policy.
There is no mutable shared catalog whose last uploader wins. Each client retains
all revisions it has observed and computes the same shared file list from them.
Provider listing visibility can delay convergence; a failed listing is not a deletion.

- Independent file edits can be published concurrently.
- Concurrent versions of one file are both retained. One deterministic version
  keeps the original path and the others get worker-named conflict paths with
  revision identifiers. This is not an automatic document merge.
- Concurrent deletion/edit retains the edited version as a named conflict copy.
  Sequential deletion is a logical shared deletion, never cloud archive erasure.
- Offline edits retain the revision that was actually present locally, not a
  newer revision merely discovered while synchronizing.
- Names, case/prefix collisions, malformed records and missing ancestors are
  checked before applying the shared tree. Unsupported combinations fail closed.

**Incoming-file safety boundary:** while a drive is mounted, RPool archives local
changes and exchanges the shared revision list, but does not replace local files
with incoming versions. Incoming additions, updates and deletions are applied at
safe unmounted sync/next startup, only when no mount lease and no persistent VFS
cache files remain. Shared mounts ask rclone to expire clean unused cache entries
on a short (5-second) polling cycle. Close applications and allow writeback/cache
expiry before unmounting. If cache remains, resume
the original mount and let rclone drain/expire it; never delete the cache to force
sync. Shared mode is eventual synchronization, not live network filesystem locking.

Displaced local bytes remain in `.rpool/shared-recovery`. An interrupted apply uses
a local journal and restores displaced bytes without overwriting newer files;
recovery may expose additional `recovered-*.bin` files for manual inspection.
Keep these backups until the shared result is checked. Do not modify the workspace
directly while an unmounted reconciliation runs.

Shared mode currently synchronizes files and their necessary parent directories;
empty directories are local-only. Renames are a new path plus deletion of the old
path. Revision history, tombstones and recovery backups are not garbage-collected.
The shared metadata directory is a required availability dependency (not sharded
across Pool providers). Existing archive data remains independently restorable.
Exchange lists validated hash-prefix pages and charges only **unseen** events
against its per-sync download budget (10,000 events / 64 MiB; 8 MiB per event).
Already-known history no longer triggers the old aggregate hard stop, and replica
sync also uses incremental fetching. Each prefix listing remains bounded by the
transport output limit. A new/long-offline client exceeding the unseen budget
still fails closed; this is **not** a scalable checkpoint/bootstrap protocol or
semantic history compaction. Local shared metadata still grows with history.
Shared workspaces upgrade their local catalog to v2; older RPool binaries refuse
them instead of silently dropping synchronization ancestry. Local-only catalogs
remain v1.

## Transfer scheduling and resilient placement

Mount writeback and shared-file downloads use the archive transfer paths of
put/get. Erasure-coded incoming files automatically use delayed hedged reads and
early group recovery; the safe unmounted-apply boundary remains unchanged. To enable strict shard distribution for a **new** workspace, select a
Pool configured with `Resilient (parity-bound)` and nonzero parity. Existing
workspaces freeze their original Pool policy; editing the Pool alone does not
change them. Empty files retain the existing no-parity handling. See README's
"Strict placement and fair transfers" for bounds and compatibility. This does not
change the deferred incoming-apply rule or turn the mount into cloud read-through.

## Local workspace durability and limits

- Keep **the entire workspace**, including `.rpool`, `files` and `vfs-cache`, on reliable
  local storage. These contain plaintext; use OS disk encryption as appropriate.
- Allow space for imported files, VFS cache, and temporary immutable upload snapshots.
  Initial imports fully download and verify selected files before exposing them.
- Each changed file creates a fresh archive version. Interrupted uploads retain their
  snapshot/identity for retry; the previously committed archive is not overwritten.
- Deletion creates a local catalog tombstone, **not remote erasure**. Prior cloud
  versions remain in inventory and continue consuming capacity. Rename currently
  archives the new path and tombstones the old one.
- In local-only mode, directory structure/empty directories and logical deletion state live in the local
  catalog. This mode is not a distributed multi-machine namespace; back up the workspace
  metadata. Individual cloud archive manifests cannot reconstruct the complete tree.
- A workspace freezes its Pool policy on creation. Later Pool configuration changes
  do not silently redistribute it. Use a new workspace to select a different policy.
- One writer owns each local workspace. Separate workspaces participate in shared
  conflict handling only when explicitly bound to the same shared encrypted folder.
  Do not edit internal metadata/transactions/cache.
- Symlinks, junctions, special files, case-colliding names and nonportable Windows names
  are rejected. Imports need a hard-link-capable filesystem (for example NTFS/APFS).
  This is not a sandbox against a hostile local process swapping filesystem paths.
- Shutdown reports pending scan changes and local VFS state when available. Open or
  cached writes may remain even after a successful final scan. Force stop retains
  cache but cannot guarantee recovery of data an application never saved.

Validation uses isolated temporary files and fake upload failures. Actual Windows
Explorer/WinFsp, macOS/Linux FUSE and live cloud mounting remain unverified here.

2026-09-28 hardening validation (macOS): **306 default tests / 317 optional
OpenDAL tests passed**, 12 external-tool tests ignored in each suite. Default
release build passed with warnings denied; release `mount --help` flags verified.
Synthetic tests cover retention crash-replay boundaries, live-reader protection,
spool budget enforcement, shared-publication protection and Resilient quota/
metadata placement. No real cloud deletion or Windows/Linux native mount test
was executed. Shared automatic retention and oversized cold bootstrap remain
unimplemented, regardless of these passing tests.

References: [rclone mount](https://rclone.org/commands/rclone_mount/),
[local backend](https://rclone.org/local/).
