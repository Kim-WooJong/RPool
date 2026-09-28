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

## Durability and limits

- Keep **the entire workspace**, including `.rpool`, `files` and `vfs-cache`, on reliable
  local storage. These contain plaintext; use OS disk encryption as appropriate.
- Allow space for imported files, VFS cache, and temporary immutable upload snapshots.
  Initial imports fully download and verify selected files before exposing them.
- Each changed file creates a fresh archive version. Interrupted uploads retain their
  snapshot/identity for retry; the previously committed archive is not overwritten.
- Deletion creates a local catalog tombstone, **not remote erasure**. Prior cloud
  versions remain in inventory and continue consuming capacity. Rename currently
  archives the new path and tombstones the old one.
- Directory structure/empty directories and logical deletion state live in the local
  catalog. This is not a distributed multi-machine namespace; back up the workspace
  metadata. Individual cloud archive manifests cannot reconstruct the complete tree.
- A workspace freezes its Pool policy on creation. Later Pool configuration changes
  do not silently redistribute it. Use a new workspace to select a different policy.
- One writer owns a workspace. Separate local workspaces are not a shared editing or
  conflict-resolution system. Do not edit internal metadata/transactions/cache.
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

References: [rclone mount](https://rclone.org/commands/rclone_mount/),
[local backend](https://rclone.org/local/).
