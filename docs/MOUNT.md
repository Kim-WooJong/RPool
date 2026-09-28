# Read/write Pool drive (local workspace)

The drive is a **full persistent local replica**, not an on-demand cloud filesystem.
Explorer/Finder writes first land in rclone's local VFS cache and workspace. RPool
periodically archives changed files to the workspace's Pool and verifies them before
committing its catalog. A successful application save does not mean cloud upload has
finished. Close files before unmounting and inspect writeback results.

## GUI

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
Exchange currently fails closed above 10,000 listed events, 8 MiB per event or
64 MiB per listing's event contents. There is no automatic history compaction yet.
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

2026-09-28 validation: default suite 211 passed / 12 ignored; optional OpenDAL suite
222 passed / 12 ignored; macOS release build passed with warnings denied. Release
`mount --help` routing was exercised. Synthetic child stop/reaping is not a real
filesystem-driver test.

Shared-mode validation (2026-09-28): default suite 226 passed / 12 ignored;
optional OpenDAL suite 237 passed / 12 ignored; default and optional macOS release
builds passed with crate warnings denied. Synthetic two-workspace tests cover
offline divergent edits, edit/delete conflicts, causal recreation, long Unicode
conflict names, directory-to-file changes, failed downloads, dirty-file refusal,
cache/lease gating and interrupted reconciliation. Real cloud exchange and
multiple-machine Explorer/WinFsp/FUSE execution remain unverified.

References: [rclone mount](https://rclone.org/commands/rclone_mount/),
[local backend](https://rclone.org/local/).
