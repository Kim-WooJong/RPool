# rpool v1.0.0

`rpool` is a Rust storage layer that stripes files across several
**explicitly supplied rclone `crypt` remotes**, optionally with
**Reed-Solomon erasure coding**, and mounts a pool as a read/write online drive
shared by several PCs. It has a CLI and a native GUI (eframe/egui).

```text
rpool CLI / GUI
   ↓
rclone crypt remotes (or RPool's own crypt-format encryption, per pool)
   ├─ Google Drive
   ├─ MEGA
   ├─ Box
   ├─ Koofr
   └─ ...
```

Every remote write goes through an rclone `crypt` remote with data encryption
enabled. Direct writes to plain provider, union or chunker remotes are rejected.
rpool does not use rclone `chunker`; it shards and adds parity itself.

Contents: [Build](#build) · [Quick start](#quick-start) ·
[Online drive](#online-drive) · [Pools](#pools) · [Archives](#archives-put-get-verify-status) ·
[Placement](#placement-and-provider-failure) · [Providers](#providers) ·
[Maintenance](#maintenance) · [Monitoring](#monitoring-mounted-pools) ·
[Portable configuration](#portable-configuration-and-crypt-secrets) ·
[GUI](#gui) · [CLI reference](#cli-reference) · [Limitations](#current-limitations) ·
[Development](#development)

## Build

```text
cargo build --release
```

The binary is `target/release/rpool`; put it in `PATH` together with `rclone`
(v1.64.0 or newer, v1.74.3 recommended). The GUI dependencies need Rust 1.95 or
later (egui 0.36).

On Windows the default `winfsp` feature builds the native WinFsp frontend; the
build machine needs the MSVC toolchain and WinFsp with its developer files
(import library under `lib\`). Without them, build with
`cargo build --release --no-default-features` (WebDAV only). The WinFsp DLL is
delay-loaded, so a PC without WinFsp still runs the binary: `--frontend auto`
uses WebDAV and `--frontend winfsp` reports that WinFsp must be installed.

## Recommended rclone layout

Create one `crypt` remote per provider, for example `gdrive-crypt:`,
`mega-crypt:`, `box-crypt:`, `koofr-crypt:`. Each can point at its
provider-specific storage directory. Do not place rclone `chunker` between
rpool and these remotes. The GUI can connect providers and create crypt
remotes for you; see [Provider setup](docs/PROVIDER_SETUP.md).

## Quick start

```text
rpool pool set archive \
  --remote gdrive-crypt:rpool --remote mega-crypt:rpool \
  --remote box-crypt:rpool --remote koofr-crypt:rpool \
  --data-shards 6 --parity-shards 2 --placement free-ratio

rpool put large.iso --pool archive            # one archive
rpool get large.iso.rpool.json restored.iso   # restore it

rpool mount --pool archive --workspace ~/rpool/ws --mountpoint ~/rpool/mnt   # online drive
rpool                                          # GUI
```

## Online drive

```text
rpool mount --pool P --workspace W --mountpoint M [--pool-worker NAME] [--frontend auto|dav|fuse|winfsp]
```

There is one drive mode: a virtual drive with automatic pool sync. The file
list comes from metadata replicated to every pool destination; bytes are
downloaded and verified on demand; writes go to a local spool and are uploaded
as verified archives in the background. Each PC uses its own workspace; no
coordinator or extra metadata service is needed. Concurrent edits keep the
original and both edited versions as named conflict copies. Close and `fsync`
are local durability points; cloud replication is asynchronous.

- Frontends: native FUSE (Linux; macOS with macFUSE, FSKit backend first, not
  yet mounted on a Mac) and WinFsp (Windows, not yet run on Windows) over
  RPool's filesystem core, or rclone mount over RPool's loopback WebDAV server
  (`dav`; on macOS via `rclone nfsmount`, used when macFUSE is missing).
- Without mounting: `--sync-only`, `--capacity-only`, `--recover-spool`,
  `--cleanup-cache`, `--apply-pool-changes` (after changing pool accounts),
  `--account-recovery-from` (copy into a new pool after losing an account),
  `--import-from REMOTE:PATH` (import files stored with plain rclone),
  `--manifest FILE` (add `put` archives to the drive).
- Local limits: `--cache-gib`, `--vfs-cache-gib`, `--cache-min-free-gib`,
  `--spool-gib`.
- Several pools can be mounted at once; a pool, workspace or mountpoint is used
  by one mount at a time.

Full description, recovery and limits: [docs/MOUNT.md](docs/MOUNT.md).

### Trash, versions and rollback

```text
rpool drive trash list|restore|purge|empty --pool P [--workspace W]
rpool drive versions list|restore --pool P ...
rpool drive rollback --pool P --at "2h ago" [--path /folder] [--confirm]
rpool drive retention show|set --pool P ...
```

Deleted files, earlier versions and rollbacks of the drive; restores and
rollbacks publish new revisions, so nothing is destroyed. Works on a mounted
drive (the mount carries out the request), an unmounted workspace, or from the
cloud without a workspace. Rollback previews unless `--confirm`. GUI: Files ›
Library (Files/Trash toggle, Versions panel, Roll back…) and Storage › Pools ›
Trash & versions. Design: [docs/DRIVE_HISTORY_DESIGN.md](docs/DRIVE_HISTORY_DESIGN.md).

### Drive metadata checkpoints

```text
rpool pool compact <POOL> [--dry-run] [--enable-deletion] [--json]
```

Checkpoints let a new PC open a drive from a few objects instead of every
metadata record. The mount maintenance loop compacts automatically. Records a
checkpoint covers are deleted only after a one-time `--enable-deletion` (older
RPool versions then stop on that pool with an error) and a grace period.
Design: [docs/METADATA_COMPACTION_DESIGN.md](docs/METADATA_COMPACTION_DESIGN.md).

## Pools

A pool stores reusable remotes and upload policy:

```text
rpool pool set NAME --remote R... [--data-shards K --parity-shards M] [--shard-mib N] \
  [--placement round-robin|free-ratio|resilient|capacity-first] [--native-crypt] [--max-object-bytes N]
rpool pool list | show NAME | remove NAME
rpool pool capacity [NAME]          # capacity with saved or unsaved options; no mount
rpool pool browse NAME              # list the drive read-only from cloud metadata
```

`put --pool` uses the pool's remotes, shard size, worker/retry settings,
placement and coding. Explicit put flags override pool defaults; `--pool` and
`--remote` are mutually exclusive. Removing a pool definition does not touch
stored shards. Capacity: [docs/POOL_CAPACITY.md](docs/POOL_CAPACITY.md).

### Pool change migration

After removing an account or changing K/M, shard size or native crypt with
`pool set`:

```text
rpool pool migrate plan <POOL>            # what moves, bytes, ETA, unrecoverable files
rpool pool migrate run <POOL> --id <ID> [--parallel N] [--stop-file PATH]
rpool pool migrate status|lost|abandon <POOL> --id <ID>
rpool pool migrate adopt <POOL> --id <ID>  # publish the migrated drive as a new generation
rpool pool migrate retire <POOL> --id <ID> [--dry-run|--confirm]   # clean up, with quarantine
rpool pool migrate restore <POOL> --id <ID> ...                    # take items out of quarantine
```

Progress lives in the cloud; rerun on any PC to resume. Kept shards are copied
server-side where the provider supports it; shards of an unreadable account are
rebuilt from K others. Nothing is deleted before `retire --confirm`, which
re-checks every reference and keeps a grace period. Design:
[docs/POOL_MIGRATION_DESIGN.md](docs/POOL_MIGRATION_DESIGN.md).

`rpool pool plan-reprocess` / `rpool pool reprocess --plan` is the older
copy-only alternative: it writes independent replacement archives and keeps
every original.

### Storage speed test

```text
rpool pool speed-test <POOL> [--size-mib N] [--files N] [--tune-uploads] [--json]
rpool provider speed-test --remote c1:rpool --remote c2:rpool [--size-mib N] [--files N] [--tune-uploads] [--json]
```

Shows which account limits a pool. Each remote gets `--size-mib` (default 16,
max 4096) of random data split into `--files` files (default one per 4 MiB;
each file at least 4 KiB), written under `<remote>/.rpool-speedtest/<run id>/`
with the pool's write path (rclone crypt, or RPool encryption for
`--native-crypt` pools), read back through rclone crypt, BLAKE3-compared and
deleted. Remotes are tested **one after another**, each with
`min(pool workers or 4, files)` parallel transfers. Reported per remote: first
operation (includes provider cold start), median latency, upload and download
rate, backend type. For a pool, the bottleneck is the account with the largest
share-to-speed ratio under its placement; the estimate is
`k/(k+m) / max(share_i / up_i)` for writes and `1 / max(data_share_i / down_i)`
for reads. A failed remote is reported and the test continues; a remote with too
little free space fails before writing. Ctrl-C (or the GUI's Stop) on
macOS/Linux stops promptly and still deletes the test files; leftovers are
listed. Data is streamed, never staged locally.

`--tune-uploads` (GUI: "Find the best number of simultaneous shard uploads")
then uploads shards (the pool's shard size; 64 MiB without a pool) to every
remote that passed at 1, 2, 4, … 32 at once (one per upload, at least two;
each level in its own folder, deleted right after; at most 64 shards per
remote, usually far fewer). The process caps are lifted meanwhile, so
throttling (a rate that stops rising) or refused concurrent writes (an error)
show. Climbing stops at the first error or after two levels without a 10%
gain. The recommendation is the smallest tested count within 10% of the best
rate; the table prints the `provider limits set --max-uploads` command, and
the GUI's Apply sets the account's Simultaneous shard uploads.

### Object size limits and native crypt

The default `64 MiB` is the plaintext **data-shard** size. rclone crypt framing
adds a 32-byte header plus 16 bytes per 64 KiB block (64 MiB → 67,125,280
bytes; 220 MiB → 230,743,072 bytes), which leaves headroom below Box Free's
250 MB file limit; up to 238 MiB still fits. Parity objects use the same size.
Pools saved with an explicit `shard_mib` keep it, and archives keep the
`shard_size` in their manifests. Shard sizes are limited to 1–4096 MiB; local
staging grows roughly with `(workers + 2·M) × shard_size`.

`--max-object-bytes 250000000` (GUI: "Provider object limit") makes pool
validation, `put --pool`, mount uploads and reprocess targets reject shard sizes
whose encrypted object would not fit. Manual `put --remote` has no pool and is
not checked.

`--native-crypt` (GUI: "Encrypt in RPool") encrypts shards inside RPool, in
rclone crypt format, and uploads them to the crypt remote's base remote; each
upload is read back through the rclone crypt remote before it counts. The crypt
remote must not use `base32768` names, `no_data_encryption` or
`pass_bad_blocks`, and its base must be a plain remote. Objects are
interchangeable with rclone crypt writes. See
[docs/NATIVE_MOUNT_CRYPT_PLAN.md](docs/NATIVE_MOUNT_CRYPT_PLAN.md).

## Archives: put, get, verify, status

```text
rpool put large.iso --remote gdrive-crypt:rpool --remote mega-crypt:rpool \
  --remote box-crypt:rpool --remote koofr-crypt:rpool --data-shards 6 --parity-shards 2 --workers 8
rpool get large.iso.rpool.json restored.iso --workers 8
rpool get "gdrive-crypt:rpool/<archive-id>/manifest.json" restored.iso
rpool verify large.iso.rpool.json [--full --workers 8]
rpool status large.iso.rpool.json [--usage]
rpool usage [--remote R ... | --manifest FILE] [--json]
```

Without `--parity-shards` an archive is plain striping (no parity). Default
placement is round-robin. Every destination receives a complete
`manifest.json` replica, so any surviving remote can restore.

- **Restore** fetches each data shard and BLAKE3-validates it. Only for missing
  or corrupt data shards does it fetch as many parity shards as needed and
  reconstruct in small stripes; a healthy restore downloads no parity.
- **Verify** checks existence and size, or with `--full` reads and hashes every
  data and parity shard. It reports missing parity as a failure even when the
  archive is still recoverable.
- **Status** reports coding, degraded/unrecoverable groups, shard health per
  remote and single-provider failure safety, for example
  `single_provider_failure_safe=true max_group_shards_on_one_provider=2 parity_budget=2`.
- **Usage** queries `rclone about --json`; with no arguments every remote from
  `rclone listremotes`. `union`, `crypt` and `chunker` layers are resolved to
  their physical backing remote; values a provider does not expose show `n/a`.

Storage overhead is `M / K`. The GF(256) backend requires `K + M <= 255`.

| Layout | Overhead | Missing shards recoverable per group | Min. balanced providers for whole-provider loss* |
|---|---:|---:|---:|
| 6+2 | 33.3% | 2 | 4 |
| 8+1 | 12.5% | 1 | 9 |
| 8+2 | 25% | 2 | 5 |
| 10+2 | 20% | 2 | 6 |
| 12+3 | 25% | 3 | 5 |

\* Assumes each group's shards are spread as evenly as possible and one provider
is the failure domain. rpool reports the actual result rather than assuming it.

### Resume

Upload writes `<source>.rpool.upload.json` (frozen placement) and
`<source>.rpool.upload.state.json` (completed shards). On resume, recorded
shards are re-checked against remote size and BLAKE3 before they are trusted.
After success the permanent manifest is `<source>.rpool.json`. Download writes
`<output>.rpool.resume.json`; only BLAKE3-validated (also reconstructed) shards
are marked complete.

### Erasure-coding notes

- Reed-Solomon over GF(256); 4 MiB encode/decode stripes by default.
- Parity shards are one full plaintext shard. A final partial data shard is
  zero-padded only for the parity math; stored data and restored length stay
  unpadded. Missing slots of a final short group are all-zero virtual shards
  and are not uploaded.
- Parity is generated one coding group at a time in temporary local files.

## Placement and provider failure

- **round-robin** (default): each coding group is balanced across configured
  remote names; separate aliases are not resolved and outage tolerance is not
  enforced.
- **free-ratio**: queries `rclone about --json`, accounts for data and parity
  bytes, first minimizes same-group shards per provider, then uses remaining
  free-space ratio. Quota pressure fails planning rather than concentrating a
  coded group.
- **resilient** (GUI: "Resilient (provider-outage bound)"): crypt/alias/chunker
  chains are resolved to backing sections, and a coding group places **at most
  M shards in one declared failure domain**. Among eligible quota domains the
  planner samples two and picks the lower projected utilization; if that
  strands later shards, deterministic greedy fallbacks retry without weakening
  the bound. If no safe plan exists, planning fails before any write. 8+2 needs
  at least five distinct resolved targets. Distinct account names are not proof
  of independent providers; unknown mappings fail closed. This protects against
  one target's loss within the coding budget, not two cloud outages.
- **capacity-first** (GUI: "Capacity-first (no provider-outage guarantee)"):
  spends the largest remaining independent account budget first. K+M coding and
  quota checks remain, but a group may have more than M shards on one provider,
  so a whole-provider outage can make an archive unrecoverable.

`put` warns when a plan places more than M shards of a group on one provider.
Changing a pool's placement affects future uploads only; use pool migration to
move existing archives. Declare shared quotas and outage groups with
`--capacity-domain`/`--failure-domain` or Storage › Pools (stored in
`provider_domains.json`). See [Pool capacity](docs/POOL_CAPACITY.md).

### Fair transfers and hedged reads

- `--workers` caps concurrent tasks per operation; with several remotes each
  gets at most `ceil(workers/2)`. This is not a limit across RPool processes.
  At most 16 rclone calls run at once per remote (`RPOOL_RCLONE_PER_REMOTE`),
  and Dropbox gets one write at a time (`RPOOL_RCLONE_WRITES_PER_REMOTE`).
- Read retries release their slot and back off (100 ms to 6.4 s, longer if the
  provider asks). Rate-limit rejections of writes are retried with backoff;
  other write failures stay "unknown outcome" and are never blindly retried.
- Data uploads overlap parity generation; staging is bounded to
  `2 * M * shard_size` plus active upload spools. Each verified parity upload is
  journaled.
- Timed-out or rate-limited reads, after retries, use parity like missing
  shards. Auth, permission, cancellation and configuration errors stay terminal.
- RS downloads use a two-group window with delayed hedges: after 2 s without
  progress (later guided by observed completion times, 250 ms–30 s) at most one
  speculative parity request per group and two globally, within the worker
  bound (`clamp(workers / 4, 1, 2)` reserved slots). A group reconstructs as
  soon as it has enough verified inputs. Two active groups bound staging to
  roughly `(workers + 2*M) * shard_size`.
- One persistent `rclone rcd` per process serves read-only calls over a private
  socket (Windows: localhost TCP with random credentials) so slow-starting
  providers pay their cold start once; writes stay subprocesses.
  `RPOOL_RCLONE_DAEMON=0` turns it off.

These are scheduling rules; tests use synthetic storage and Docker, and
real-cloud numbers are in the CHANGELOG.

## Providers

```text
rpool provider health [--pool P] [--json]
rpool provider limits show|set|reset|bandwidth|keepalive-days|default-uploads ...
rpool provider keepalive ...
rpool provider ensure-encryption | encrypt --name N --provider P ...
rpool provider drain FILE.rpool.json --from old-crypt:rpool --to new-crypt:rpool [--dry-run] [--delete-source] [--allow-risky]
rpool remote-root list | set REMOTE PATH | remove REMOTE
```

- **health** checks each provider at its encrypted remote root (`name:`), with
  quota where `rclone about` works. `--pool` only selects providers.
- **limits**: per-account rolling 24 h upload budget (Google Drive default
  750 GB; others none), shared by every RPool process on the PC; when exhausted,
  uploads to that account wait instead of failing. Last activity per account
  with inactivity warnings, a bandwidth timetable in rclone syntax
  (`"08:00,512k 18:00,30M"`, `UP:DOWN`) and per-account `--tpslimit`.
- **Concurrency is counted in shards.** One shard upload is one upload
  request: RPool sets rclone's per-file chunk concurrency to 1 for its uploads
  (`RCLONE_CONFIG_<ACCOUNT>_UPLOAD_CONCURRENCY=1`, overriding rclone.conf;
  e.g. Filen's default of 16), so nothing multiplies the counts below.
  - `default-uploads N` (GUI: Settings › Network): simultaneous shard uploads
    per account for accounts without their own value (`0` = built-in 16;
    Dropbox stays 1).
  - `set --remote R --max-uploads N` (GUI: provider card › Limits): this
    account's own value, which wins (`0` = the default above).
  - The drive's shard transfers of this PC are a drive option
    (`mount --workers N`, GUI: Drive › Cache & pending writes); without it the
    pool's saved `workers` apply. Changing it keeps started uploads resumable.
- **keepalive** makes one cheap authenticated call per account and records it
  as activity; mounts do it automatically after `keepalive-days`. Providers
  decide what counts as activity.
- **encrypt / ensure-encryption** create crypt remotes with OS-generated keys;
  existing keys are never rotated. New crypt folders start under the
  provider's remote default path. See [docs/PROVIDER_SETUP.md](docs/PROVIDER_SETUP.md).
- **drain** copies one archive's shards to another provider, fully verifies
  them, rewrites and replicates the manifest, and only then deletes the source
  if `--delete-source` is given. Weakening single-provider failure safety needs
  `--allow-risky`. Drive archives reject `--delete-source`.
- **remote-root** stores per-remote default paths (`remote_roots.json`): with
  `rpool remote-root set Instance /data/crypt`, a bare `Instance:` resolves to
  `Instance:/data/crypt` for storage and capacity lookups; an explicit path wins.
  It does not rewrite rclone crypt configuration. Paths inside rclone virtual
  remote configuration (`remote = Instance:/data/crypt`) are preserved for quota
  lookup.

## Maintenance

```text
rpool scrub FILE.rpool.json [--quick] [--repair]
rpool repair FILE.rpool.json [--dry-run] [--group N ...]
rpool manifest verify|replicate FILE.rpool.json
rpool manifest recover <archive-id> --pool P
rpool inventory add|rebuild|list|find|info ...
rpool history list --limit 50 | prune --keep 500
rpool doctor [--local-only] [--json] [--bundle rpool-diagnostics.zip]
```

- **Scrub** streams every shard and compares BLAKE3 (quick: existence and size)
  and separates missing, wrong-size, corrupt and transport-error shards.
  **Repair** reconstructs recoverable data and parity shards, validates them,
  uploads to the original object path and verifies again; a transport error is
  never treated as an erasure. Plain archives cannot regenerate missing data.
- **Manifest** verify compares replicas by semantic fingerprint; replicate
  repairs or adds copies (original JSON bytes preserved); recover finds a valid
  replica and writes a local `.rpool.json`.
- **Inventory** is a rebuildable local index, not the source of truth.
  **History** stores compact JSONL records without raw command lines or
  credentials.
- **Doctor** checks rclone (`rclone-support`: FAIL below v1.64.0, WARN below
  v1.74.3, which fixes CVE-2026-49980 in `--rc-serve`), remotes, pools,
  configuration paths, inventory, history and drive metadata growth.
  `--bundle FILE.zip` (GUI: Settings › General › Export diagnostics…) writes a
  redacted diagnostics archive: versions, the doctor report,
  `rclone config redacted` (redacted again by RPool), RPool settings without
  secret fields, mount registry entries, bounded tails of the history and mount
  logs, and monitoring status. `manifest.txt` lists every file and the
  redaction rules. The rclone config file, crypt passwords, tokens, `.env`
  files, age identities and file contents are never included. Review before
  sharing.

## Monitoring mounted pools

```text
rpool mount monitor [--workspace DIR | --pool NAME] [--watch] [--json] [--history-minutes N]
```

Every running `rpool mount` (CLI or GUI) counts its own cloud traffic per
account and registers itself in `<config dir>/mounts/`. `rpool mount monitor`
lists running mounts with uptime, upload queue (files, bytes, oldest), last
metadata sync and alerts, plus one row per account: rate over 10 s, totals,
active transfers, ok/failed operations, rate-limit retries and the last error.
`--watch` refreshes every second; `--json` prints `[{entry, status}]` (with
`--history-minutes` also `history`). GUI: the **Monitoring** page.

- **sent**: bytes handed to rclone for uploads; **acked**: uploads rclone
  finished; **verified**: bytes read back after an upload; **received**: bytes
  downloaded (reads, readbacks, listings). Native-crypt pools count ciphertext
  for sent/acked. Totals start with the mount process. Server-side copies count
  as operations without bytes.
- Alerts: *stalled* (uploads pending, nothing sent for 180 s), *errors*
  (failures in the last minute), *unreachable* (an account only failed for a
  minute), plus upload-limit and metadata-growth badges.
- Files: `.rpool/net-status.json` (rewritten every second, removed on clean
  unmount; older than 10 s means not live) and
  `.rpool/net-history/YYYY-MM-DD.jsonl` (UTC days, per account and minute, kept
  90 days). Pools that are not mounted are not monitored.

## Portable configuration and crypt secrets

```text
rpool export ./managed/rpool --age-recipient <age-recipient>
rpool import ./managed/rpool --age-identity ~/.config/age/rpool.key --dry-run
rpool import ./managed/rpool --age-identity ~/.config/age/rpool.key
rpool config paths
```

`rpool export` writes a fixed artifact tree:

```text
rpool/
├─ config/portable-config.json   # pools, remote roots, portable GUI defaults, vault binding
└─ secrets/rclone.age            # age-encrypted crypt password/password2 (still obscured)
```

`rpool import` validates the whole artifact, then restores the portable settings
and the exact obscured `password` / optional `password2` values
transactionally; rpool never deobscures them. The target rclone config must
already contain the same named `crypt` remotes with matching non-secret
structure; provider credentials and tokens are outside this feature. Export
requires `--age-recipient` when crypt remotes exist; import requires
`--age-identity` (the rollback recipient is derived with `age-keygen -y` unless
given). The age identity stays outside the artifact. Inventory, history and
journals are not exported, and the local rclone executable path is kept on
import. `rpool config paths` prints the active settings paths as JSON.
`examples/dot-rpool-sync.nu` wraps export/import for a dotfiles workflow.
Details: [docs/CRYPT_SECRET_PORTABILITY.md](docs/CRYPT_SECRET_PORTABILITY.md).

## GUI

`rpool` with no subcommand (or `rpool gui`) opens the native console. Global
options still apply, for example `rpool --rclone C:\Tools\rclone.exe`. On
Windows the GUI runs as a detached no-console child.

- **Overview**: capacity, files, health, pools, warnings, recent jobs.
- **Files**: Library (file-explorer view of the drive, Trash, Versions,
  Roll back), Upload, Restore.
- **Drive**: mount, sync, import, recovery and cache settings per pool;
  several pools mounted at once with a "Mounted pools" strip.
- **Monitoring**: live traffic per mounted pool and account, history.
- **Storage**: Providers (cards with usage, limits, keep-alive, speed test),
  Pools (policy, capacity, drive metadata, trash & versions, speed test),
  Account changes (pool migration wizard).
- **Health**: Archive (verify/status), Integrity, Metadata, Diagnostics.
- **Activity** (jobs and history) and **Settings** (General, New-pool defaults, Encryption & paths,
  Network, Portable configuration).

Storage operations run the same CLI code as a child process
(`RPOOL_PROGRESS_PROTOCOL=1`), so GUI and CLI behave alike. GUI settings live in
`gui.json` next to the other settings (`rpool config paths`; Windows
`%APPDATA%\rpool`, macOS `~/Library/Application Support/rpool`, Linux
`$XDG_CONFIG_HOME/rpool` or `~/.config/rpool`).

## CLI reference

| Command | Purpose |
|---|---|
| `gui` | Native console (default without a subcommand) |
| `mount` | Online drive; also `mount monitor` |
| `drive trash\|versions\|rollback\|retention` | Drive history |
| `put` / `get` | Upload / restore one archive |
| `verify` / `status` / `usage` | Shard checks, recoverability, quotas |
| `pool set\|list\|show\|remove\|capacity\|browse` | Pool definitions and capacity |
| `pool migrate plan\|run\|status\|lost\|abandon\|adopt\|retire\|restore` | Pool change migration |
| `pool plan-reprocess` / `pool reprocess` | Copy-only reprocessing |
| `pool speed-test` / `pool compact` | Speed test, metadata checkpoints |
| `provider health\|limits\|keepalive\|speed-test\|encrypt\|ensure-encryption\|drain` | Provider operations |
| `remote-root list\|set\|remove` | Per-remote default paths |
| `manifest verify\|replicate\|recover` | Manifest replicas |
| `inventory add\|rebuild\|list\|find\|info` | Local archive index |
| `history list\|prune` | Operation history |
| `scrub` / `repair` | Integrity scan and repair |
| `doctor` | Diagnostics, `--bundle` |
| `export` / `import` / `config paths` | Portable configuration |

`rpool <command> --help` shows every option.

## Compatibility

New archives use manifest version 2; v0.1 manifests remain readable for
restore/verify/status, and interrupted v0.1 upload plans can resume. Integrity
and provider-maintenance features use the existing manifest metadata. The
preserved formats are listed in
[docs/architecture/compatibility.md](docs/architecture/compatibility.md).

## Current limitations

- Automatic repair restores shards to their manifest-declared paths. To
  replace an account, remove it from the pool and run `rpool pool migrate`.
- Crypt remotes resolving to the same backing remote count as one failure
  domain. Two backing remotes of the same cloud account are not detected;
  declare them as capacity/outage identities.
- Pool sync is eventual: no distributed locking, no cross-PC open-handle
  coherence, empty folders stay local. Drive payload history accumulates until
  it is reclaimed.
- Several pools can be mounted at once, but a pool, workspace (also nested) or
  mountpoint is used by one mount at a time.
- The WinFsp frontend, the Windows seal/flush fix and the Windows rclone daemon
  path are compiled for Windows but not yet run on Windows. macOS has no
  native frontend (WebDAV over NFS).
- Network monitoring covers mounted pools only, per mount process. Speed tests
  run remotes one after another; prompt Ctrl-C cleanup is described for macOS
  and Linux only.
- On bucket backends `put` to crypt still fails (a missing key stats as a
  directory).
- `rclone about` support varies by backend; unsupported values show `n/a`.
- Reed-Solomon protects shards inside each coding group. It does not replace
  backups, account recovery or independent copies of the manifest.

## Development

Build, test and repository rules: [DEVELOPMENT.md](DEVELOPMENT.md). Local CI:
`scripts/ci-local.sh`; Linux FUSE end-to-end tests in Docker:
[scripts/linux-docker/README.md](scripts/linux-docker/README.md). Plan:
[ROADMAP.md](ROADMAP.md); release notes: [CHANGELOG.md](CHANGELOG.md); version
policy: [docs/VERSIONING.md](docs/VERSIONING.md).
