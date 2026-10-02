# Native mount with rclone-compatible crypt

Updated: 2026-10-01. Plan and milestone status. It extends Phases 2 and 3 of
`MOUNT_WRITE_ROADMAP.md` and does not replace them. The drive has one mode,
the virtual drive with v6 pool sync (`docs/MOUNT.md`); status notes below that
were written while other modes existed are updated to that state.

## Goal

Mount an RPool drive with RPool's own OS frontend, without rclone mount, rclone
VFS or the loopback WebDAV server. Encryption must work the way it does with
rclone `crypt` today:

- Existing archives stored through rclone crypt remotes stay readable.
- Objects written by RPool stay readable with the same rclone crypt remote.

## Layering

```text
OS  ──  WinFsp (Windows) │ FUSE (Linux) │ macFUSE or NFS (macOS)     frontends
          │
     filesystem core: lookup/open/read_at/write_at/truncate/flush/release
          │                                            (roadmap Phase 2)
     VirtualDrive: namespace, revisions, spool, quota, peer/pool sync
          │
     storage backend (per pool)
       ├─ rclone crypt remote (current behaviour; the default)
       └─ native crypt ─ transport to the crypt's *base* remote
                          └─ rclone (subprocess writes; persistent `rclone rcd` for reads)
```

The frontend and crypt are independent layers. A native mount can still use
rclone crypt remotes, and native crypt can still sit behind the WebDAV
frontend. Each layer ships and is verified separately.

## Crypt design decisions

- **Format.** Byte-compatible with rclone `backend/crypt`:
  - scrypt key derivation (N=16384, r=8, p=1) into 80 bytes: data key, name key and name tweak.
  - Header `RCLONE\0\0` followed by a 24-byte nonce.
  - XSalsa20-Poly1305 secretbox over each 64 KiB block.
  - Name encryption: standard (EME-AES-256 with base32hex, base64 or base32768), obfuscate, or off.
- **Keys.** Read from the existing crypt remote through `rclone config dump`, which `config_sync` already does. They are revealed in memory only and never logged or persisted. RPool never writes `rclone.conf`.
- **Unsupported settings are refused, never silently mishandled:** `no_data_encryption`, unknown modes or encodings, and nested remote chains.
- **Compatibility is enforced by tests:** rclone's published vectors, plus round-trip oracle tests against the installed rclone on a local directory in both directions. No cloud access is needed.
- **The existing write gate stays.** Uploads must still land on encrypted storage. With native crypt, that guarantee moves from "destination is a crypt remote" to "RPool encrypted the bytes".

## Milestones

| # | Deliverable | Exit gate |
| --- | --- | --- |
| M1 ✅ | `src/crypt`: format library (keys, reveal, streaming data, names, sizes, ranged reads) | Two-way rclone oracle tests pass (rclone 1.75.1, 2026-09-29). No production caller yet. |
| M2 ✅ | Native-crypt storage backend over the base remote. Opt-in per pool. | Objects are interchangeable with the rclone crypt remote. Ranged reads verified. Fault tests pass. |
| M3 ✅ | Protocol-independent filesystem core (roadmap Phase 2) | The Phase 2 exit gate in `MOUNT_WRITE_ROADMAP.md` |
| M4 (built, not run) | Windows WinFsp frontend (`winfsp_wrs`, MIT). Read-only first, then writable. | Windows machine: listing, reads, stop, and small writes with remount and recovery |
| M5 ✅ | Linux FUSE (`fuser`) | Linux machine (Docker Linux VM: kernel tests pass) |
| M6 (built, not mounted) | macOS frontend: the FUSE adapter over macFUSE (libfuse loaded at runtime, FSKit backend first, kernel backend fallback) | New workspace. Clean and uncertain stop measured. Needs macFUSE's file system extension enabled on the test Mac. |

`--frontend auto` (default since 2026-09-30) picks the native frontend where
the build has one and falls back to WebDAV (see "Native by default"). The
fresh 4 GiB 9+3 cloud round-trip gate in the roadmap is still open for M4.

## M1 status (2026-09-29)

Done in `src/crypt`: obscure/reveal, scrypt keys (default salt or `password2`),
streaming encrypt/decrypt with 64 KiB secretbox blocks, ranged reads from any
plaintext offset, encrypted/decrypted size, and names in `standard` (base32 or
base64, with or without directory name encryption), `obfuscate` and `off`
modes. `CryptConfig` parses a `rclone config dump` crypt section.

Verified by `cargo test --bin rpool -- --ignored crypt::oracle` against the
installed rclone on local directories: obscure both ways, names identical to
`rclone cryptdecode --reverse`, objects written by rclone decrypt in RPool
(including ranged reads), and objects written by RPool read back with
`rclone cat`/`lsf`. No cloud access.

Refused, not implemented: `filename_encoding = base32768`,
`no_data_encryption`, `pass_bad_blocks`, and unknown options. Version-suffixed
names (`--b2-versions`) are not handled. Name decoding is stricter than Go's
about non-canonical trailing bits; rclone never produces those.

Instead of the published rclone test vectors, the rclone binary itself serves
as the reference.

## M2 status (2026-09-29)

Done for `put` and pool reprocess. A pool opts in with `native_crypt`
(`pool set --native-crypt`, GUI "Encrypt in RPool"). Shards are then encrypted
by `CryptBackend` and written with `rclone rcat` to the crypt remote's base.
The writer's readback still goes through the rclone crypt remote, so every
native write is proven readable by rclone before it counts.

The write gate (`src/storage/native_crypt/route.rs`) allows a destination only
when it is a configured crypt remote that `CryptConfig` fully supports, its
base is a plain configured remote (not crypt, alias, union, combine, chunker,
hasher, compress or cache, and not a `:backend:` string), and no
`RCLONE_CRYPT_*` or `RCLONE_CONFIG_*` override except `RCLONE_CONFIG_PASS` is
set. Crypt options that are present with an empty value are refused rather
than read as defaults. Keys that are non-ASCII or not canonical (`a//b`, `./a`,
`a/`) use the gated rclone crypt route instead. The config dump is read once
per command, and failures are not cached.

Verified: gate unit tests, the M2a fault and ranged-read tests, and an ignored
end-to-end test (`native_writer_objects_read_back_through_rclone_crypt`) in
which a native writer publishes onto a local base, `rclone cat` returns the
same bytes, and the base holds only `RCLONE\0\0` ciphertext under encrypted
names. No cloud provider was used.

Not covered at M2: mounts, repair, scrub, migrate and manifest replication
still wrote through rclone crypt (mounts were added later, see below). Objects are interchangeable, so mixing is safe.
Server-side copy and rename through `CryptBackend` are not wired.

## M3a status (2026-09-30)

`src/mount/fs_core/` provides `FsCore` over `VirtualDrive`: `lookup`,
`readdir`, `open`, `read_at`, `write_at`, `truncate`, `flush`, `fsync`,
`freeze`, `release`, `rename`, `delete`, `mkdir`, `rmdir` and `statfs`.

| Operation | Local durability ack | Cloud sync |
| --- | --- | --- |
| `write_at`, `truncate` | none (volatile; spool budget enforced) | none |
| `fsync`, `flush`, `freeze`, last write `release` | yes: `VirtualDrive::seal` (spool fsync, `intent.json`, namespace save) | eligible for the next `sync`; never waited on |
| `rename`, `delete`, `mkdir`, `rmdir` | yes: the drive's atomic namespace save (unsealed rename sources are sealed first) | eligible |

- All write handles of a file share one generation. The first mutation of an
  existing file copies its current visible revision (read under the file's
  generation lock, so after any concurrent seal) into a new spool image.
- A generation that starts from a revision sealed earlier in this workspace
  records it as `depends_on`, so successive saves commit as one history
  rather than as conflicting siblings. The DAV route keeps its conservative
  sibling behaviour, because a DAV PUT carries no trusted continuation.
- A read-only handle opened while the file has no unsealed writes reads an
  immutable snapshot. Other handles follow this core's seals.
- Unlink or rename-over leaves open handles working. `fsync` of the replaced
  file returns `Stale`, and its unsealed spool is discarded at last release.
- File identity is per session and not persisted; there is no new on-disk
  format. Pool-sync workspaces were refused at M3a; they are served since
  "Native by default" below.

Verified by `cargo test --bin rpool fs_core` on a fixture workspace (no OS
mount, rclone or network): local acknowledgement without upload, abrupt exit
and reopen, old readers across overwrite/rename/delete, concurrent writers,
rename-over, delete/recreate, retries, spool budget, truncate and sparse
writes, open flags and directories, and successive saves plus a rename
committed in `sync` order without conflict copies (this test fails without
the `depends_on` continuation). An independent adversarial review found a
stale-base race between a write and a concurrent seal; it is fixed, but only
the sequential case is tested.

Known limits: the first write to a clean cloud file hydrates it while holding
that file's generation lock and the shared namespace lock, so a stalled
download delays namespace changes. Every lookup and attached read rebuilds the
drive view (O(files)). A failed `write_at` may leave a prefix that a later
`fsync` seals, as POSIX allows.

M3b is done (see below).

## M3b status (2026-09-30)

- **Crash points.** `src/mount/crash.rs` (test-only; `Ok` in release builds)
  marks spool writes (cut half-way), the seal steps (before fsync, after the
  intent record, before and after the namespace save), the namespace backup
  and atomic-persist steps, and the rename and delete saves. The matrix test
  crosses 9 points with overwrite, create, rename and delete. The simulated
  process stops at its first failure without releasing handles. After a
  reopen from disk, acknowledged operations are always present, unacknowledged
  ones are entirely old or entirely new, sealed images keep their hashes, and
  the workspace keeps working.
- **Randomized traces.** 24 fixed seeds × 60 operations (open, write,
  truncate, read, fsync, release, rename, delete, crash) are checked against a
  reference model after every step: result kinds, sizes and bytes of every
  path, and snapshot/attached reads. At the end all intents are committed in
  `sync` order and there must be no conflict copies. The traces found two
  defects, both fixed: `rename(x, x)` of a missing file returned Ok, and a
  file recreated after a pending delete became a sibling of the deletion.
  Generations now continue the latest pending intent at their path.
- **Stalled uploader.** While the sync gate is held as a stalled upload,
  writes, fsync, rename, delete and mkdir all complete.
- **DAV stays on its own path.** FsCore treats a later save of the same file
  as a trusted continuation. A DAV PUT carries no such identity (roadmap
  Phase 1, Case B), so moving DAV onto FsCore would change its conservative
  behaviour. The native frontends (M4, M5) use FsCore directly.

Not covered: a real upload through rclone interleaved with writes (the fixture
has no remote), power loss below the filesystem, and multi-process access.

## M5 status (2026-09-30): Linux FUSE

`src/mount/frontend/fuse/` implements `fuser::Filesystem` (fuser 0.18, pure-Rust
mount, no libfuse) over `FsCore`. `rpool mount --frontend fuse` (optionally
`--native-read-only`) mounts a drive workspace with it.
The shared lifecycle in `src/mount/frontend/run.rs` runs background `sync`, and
it unmounts when the stop file appears or when the OS unmounts the filesystem.

- Inode numbers map to paths (root is 1). Rename moves numbers and unlink
  forgets them, so a recreated file gets a new inode. The attribute and entry
  TTL is 1 s.
- `flush` (every `close`) and `fsync` seal. `release` closes the core handle.
  `setattr(size)` truncates through the open handle, or through a temporary
  write handle that is sealed at once. Mode, owner and times are not stored.
- `RENAME_NOREPLACE` is honoured; `RENAME_EXCHANGE` returns `EINVAL`. A
  read-only mount returns `EROFS`.
- Errors map as NotFound→ENOENT, Exists→EEXIST, NotEmpty→ENOTEMPTY,
  NoSpace→ENOSPC, Stale→ESTALE and I/O→EIO (logged).

Verified on Linux 6.12 (Docker Desktop linuxkit VM, `--device /dev/fuse
--cap-add SYS_ADMIN`; not this Mac's kernel). The script
`scripts/linux-docker/run-tests.sh` runs `cargo fmt --check`, `cargo
check --all-targets` with warnings denied, the ignored kernel tests and the
whole suite. The kernel tests cover:

- a 3 MiB file round trip and persistence across remount
- partial overwrite, `set_len` and append
- rename-over and directory rename
- ENOTEMPTY and O_EXCL
- an open reader keeping its bytes after the file is replaced and unlinked
- eight concurrent writers
- EROFS on a read-only mount

The whole Linux suite passes (505 passed, 26 ignored).

The CLI end-to-end test (`scripts/linux-docker/fuse-e2e.sh`, rclone v1.75.1)
also passes. It uses three crypt remotes over local directories, RS 2+1, 1 MiB
shards, and declared capacity and failure domains, and it checks the
following:

- a 3 MB file plus a renamed file in a subdirectory are readable while
  mounted;
- background sync uploads all 5 pending intents, leaving 22 encrypted objects
  that contain no plaintext;
- the stop file unmounts cleanly;
- after a remount the data reads back through the committed cloud revisions.

The Docker image and scripts are in `scripts/linux-docker/`.

Not yet verified: mmap-heavy applications, very large files (cloud hydration
while locks are held), real cloud providers, and multi-user `allow_other`.
Without `--capacity-domain`/`--failure-domain` declarations, sync keeps the
data local ("No quota-known upload targets"), as with the DAV route.

## M4 status (2026-09-30): Windows WinFsp, compiled but not run

`src/mount/frontend/winfsp/` implements `winfsp_wrs::FileSystemInterface`
(winfsp_wrs 0.4.1, MIT) over `FsCore`. The `winfsp` feature is on by default
and only has an effect on Windows, where linking needs WinFsp and its SDK
import library installed (otherwise build with `--no-default-features`);
`build.rs` delay-loads `winfsp-x64.dll`, and `winfsp_wrs::init()` loads it from
WinFsp's install directory at mount time.
Usage: `rpool mount --frontend winfsp --mountpoint R: ...`.

- **Contexts.** A per-open integer key (Descriptor mode) is used, never a
  pointer, so a volume-level Flush (NULL context) is safe.
- **Durability.** The volume flushes and purges on Cleanup, so cached writes
  reach RPool first. Cleanup and Flush seal the file, and Close releases the
  core handle. Cleanup cannot report errors: they are logged, and the bytes
  stay in the spool.
- **Delete.** `set_delete` vetoes non-empty directories; Cleanup with the
  DELETE flag unlinks.
- **Rename.** Rename honours `replace_if_exists`, refuses to replace a
  directory, allows case-only renames, and moves open contexts along.
- **Case.** The volume is case-insensitive and case-preserving: names resolve
  onto existing entries regardless of case (`frontend/names.rs`), and the
  drive itself refuses names that differ only in case.
- **Writes and allocation.** `WriteToEOF` appends. `ConstrainedIO` (paging)
  writes are clipped at EOF. An allocation below the file size truncates.
  Attributes and times are not stored.
- **Security.** One security descriptor gives full access to SYSTEM,
  Administrators and Everyone.

Verified on macOS only:

- `cargo check`/`clippy --target x86_64-pc-windows-gnu --features winfsp`
  pass with no warnings in the frontend.
- The platform-independent rules pass unit tests: case-insensitive
  resolution, resuming a listing after a marker in one bytewise order, and
  paging-write clipping.
- `FsCore::stat` (per-handle attributes, including after unlink) is covered
  by core tests.

**Not verified:** any WinFsp runtime behaviour on Windows (mount, Explorer,
Office-style save patterns, cached/paging I/O, delete-on-close, stop). There
is no hosted CI. The M4 exit gate is still open: a Windows machine
must pass listing, reads, stop and small writes with remount and recovery.

Testing also found and fixed a core defect: when the final seal at
last-handle close failed, a phantom unsealed file stayed visible. The
generation is now dropped from memory and its spool remains on disk for
recovery.

## Native by default (2026-09-30)

- **Frontend `auto` is the default in the CLI and GUI.** It uses the native
  frontend where one exists for the build, otherwise WebDAV:
  - Linux: FUSE.
  - Windows: WinFsp, in `winfsp` builds only.
  - macOS: FUSE via macFUSE when installed (built, not yet mounted), else WebDAV.
- **Explicit choices still apply.** `--frontend dav|fuse|winfsp` is honoured,
  and an explicit native choice the build cannot serve is refused.
- **Pool sync v6 is served natively.** Design review by an independent
  expert. The changes:
  - Native opens protect served revisions from retention without the DAV
    range fence (`observe_open`).
  - Attached handles of one file share one base. A peer revision reaches
    them only after all of them are closed, and snapshot handles never
    change.
  - **An edit descends from the bytes it was built on.** A partial edit keeps
    its base revision as ancestry. A truncating save or delete descends from
    the revision last read (persisted with `observe_read`), else from this
    workspace's latest pending intent at the path, else, for a recreate,
    from the events that ended the path's history.
  - A peer's concurrent edit therefore becomes a preserved v6 conflict
    (original plus both named edits) instead of being overwritten.
- **Tests.** Nine peer scenarios cover reopening own writes, handles across
  peer updates, partial and truncating saves, own chains, deletes, a peer
  delete while open, rename and conflict listing. The crash matrix and the
  randomized traces also run in pool-sync mode. The traces found that
  recreating after a pending delete started a new root; fixed.
- **Leftover rclone cache.** A previous WebDAV session's rclone VFS cache is
  set aside, not deleted, before a native mount; it is never replayed.
- **Native crypt covers mounted drives.** New GUI pools default to it.
- **FUSE seals on `release` and `fsync`.** `flush` runs on every close of
  every duplicate descriptor, so sealing there sealed an empty file when a
  shell did `open; dup2; close` before writing.
- **Kernel-level check:** `scripts/linux-docker/pool-sync-e2e.sh` mounts one
  pool from two workspaces in Docker FUSE and passes (2026-09-30):
  propagation, an atomic save from B, a delete, a sequential edit with no
  conflict copy, concurrent edits kept as the original plus both named
  copies, clean stops, and no plaintext content or names in the remotes.
