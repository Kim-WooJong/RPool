# rpool v0.5.15

## Next / unreleased: storage pivot

Storage I/O now uses verified Reader/Writer services behind the synchronous StorageBackend contract. Default production access remains rclone crypt; OpenDAL is an optional test-only Memory prototype. Windows is the primary target, but current executed validation is macOS only. See [verification](docs/architecture/verification.md) and [limitations](docs/architecture/limitations.md). Version 0.5.16 remains reserved for Settings/UI Persistence.

`rpool` is a small Rust storage layer that stripes large files across multiple **explicitly supplied rclone `crypt` remotes** and can optionally add **Reed-Solomon erasure coding**.

```text
Nushell
   ↓
rpool
   ↓
rclone crypt remotes
   ├─ Google Drive
   ├─ MEGA
   ├─ Box
   ├─ Koofr
   └─ ...
```

Remote storage is now **enforced** through rclone `crypt` remotes. Direct writes to plain provider, union, or chunker remotes are rejected. rpool does **not** require or use rclone `chunker`.

## Crypt Secret Portability

`rpool export` creates a portable artifact tree containing ordinary rpool settings plus an age-encrypted vault for the already-obscured rclone `crypt` passwords. `rpool import` validates the complete artifact, restores portable rpool settings, and restores the exact obscured `password` / optional `password2` values transactionally. rpool never deobscures those values.

```text
rpool export ./managed/rpool --age-recipient <age-recipient>
rpool import ./managed/rpool --age-identity ~/.config/age/rpool.key --dry-run
rpool import ./managed/rpool --age-identity ~/.config/age/rpool.key
```

The artifact layout is fixed:

```text
rpool/
├─ config/
│  └─ portable-config.json
└─ secrets/
   └─ rclone.age
```

`portable-config.json` contains only portable structure and a BLAKE3 binding to the encrypted vault. `secrets/rclone.age` contains the crypt secret bundle under age encryption. The age identity/private key must remain outside the artifact root and is never exported. If `--rclone-config` is omitted, rpool asks rclone for its active config path. During import, the target config must already contain the same named `crypt` remotes with matching non-secret structure; provider credentials and tokens remain outside this feature.

When crypt remotes are present, export requires `--age-recipient`. Import requires `--age-identity`; the public recipient used for rollback snapshots is derived with `age-keygen -y` unless `--age-recipient` is supplied explicitly. The legacy `rpool config export/import` JSON-only commands remain available but intentionally do not carry crypt secrets.


## v0.5.14 storage workspace

Patch 8 consolidates Storage around provider and pool operations. Providers are shown as physical capacity targets with reachability and latency, while the details pane exposes linked crypt remotes, backing paths, default roots, and quota data. Pools use a list/details workflow with Reed-Solomon policy, placement, unique physical failure domains, estimated usable capacity, and single-provider failure-safety analysis.

Provider migration no longer exposes a direct destructive action. The GUI first runs the real `provider drain --dry-run` backend path, displays the resulting shard move plan, requires validation to succeed, then requires explicit confirmation before execution. Changing the manifest, source, destination, output path, deletion policy, or risky-safety override invalidates the preview and forces validation again. Source deletion remains opt-in and occurs only after copied shards are fully verified and the updated manifest has been written and replicated.

On Windows, GUI launches use a detached no-console child while explicit CLI commands retain normal terminal behavior.

## v0.5.12 build hotfix and update cleanup

- Fixed GUI startup type inference by explicitly collecting configured pool names into `Vec<String>`.
- Updated desktop drag-and-drop handling for egui 0.36.2 to use the `DroppedFile::path()` method.
- Added `scripts/update-cleanup.nu` for safe in-place upgrades. It removes only explicitly listed GUI source files retired by earlier file-to-folder module migrations and supports `--dry-run`.

For an in-place update, after extracting the new version over the project directory, run:

```text
nu scripts/update-cleanup.nu --root . --dry-run
nu scripts/update-cleanup.nu --root .
```

A clean checkout/extraction does not require the cleanup step.

## v0.5.11 module-layout hotfix

Folder-based GUI modules that replaced older single-file modules now use explicit `#[path = ".../mod.rs"]` declarations. This prevents Rust `E0761` ambiguity when an in-place ZIP update leaves legacy files such as `dashboard.rs`, `maintenance.rs`, `inventory.rs`, `upload.rs`, or `state.rs` beside their newer module directories. Storage and GUI workflows are otherwise unchanged.


## v0.5.10 upload workflow

Patch 6 of the staged GUI redesign simplifies Files > Upload around storage pools and a visible file queue. Multiple files can be selected or dropped into the window and are uploaded sequentially through the existing `rpool put` command path. Normal uploads select a named pool; one-off destinations and shard/worker/retry/placement/Reed-Solomon controls live under a collapsed **Advanced** section.

Before an upload starts, the GUI shows a compact preflight summary for pending files, target, coding policy, and encrypted destination count. Invalid pool definitions, missing source files, invalid RS parameters, or destinations that are not recognized as safe crypt remotes block the Start button. If crypt discovery is temporarily unavailable, the GUI reports a warning while the lower storage layer still performs the authoritative crypt check before every write.

## v0.5.9 files workspace

Patch 5 of the staged GUI redesign turns the Files > Library view into the primary inventory browser. It loads the local rebuildable inventory cache directly, supports search, pool/erasure-coding filters, sortable columns, file selection, and a persistent details pane. Restore, Verify, and Status actions reuse the selected archive manifest and existing command workflows. Inventory metadata does not claim live remote health: entries remain "Not checked" until the operator opens Status or Verify.

## v0.5.8 dashboard

Patch 4 of the staged GUI redesign replaces the old provider-card overview with a compact desktop dashboard. The first screen now summarizes known physical capacity, indexed file count/logical size, overall attention state, provider usage, pool readiness, warnings, and the five most recent jobs. Dashboard rows link into Storage and Jobs for detailed operations; backend storage behavior is unchanged.

## v0.5.7 GUI theme foundation

Patch 3 of the staged GUI redesign centralizes the desktop visual foundation without changing storage workflows. Shared spacing, control height, restrained corner radius, semantic status colors, exact capacity bars, section headers, and toolbars now have single owners under `src/gui/theme.rs` and `src/gui/widgets/`. The application keeps the current light/dark theme rather than applying a decorative custom palette.

## v0.5.6 GUI navigation consolidation

The GUI now uses six top-level work areas: **Dashboard**, **Files**, **Storage**, **Jobs**, **Maintenance**, and **Settings**. Existing feature screens are grouped behind local section tabs, so the sidebar exposes workflows rather than individual commands. Files contains Inventory/Upload/Restore/Verify/Status; Storage contains Providers/Pools; Maintenance contains Integrity/Metadata/Diagnostics. Storage behavior and CLI operations are unchanged. The staged redesign plan is documented in `GUI_REDESIGN.md`.
On Windows, GUI launches are detached into a no-console child process; CLI commands keep normal terminal behavior.

## v0.5.4 default GUI startup

Running `rpool` with no subcommand now opens the native GUI:

```text
rpool
```

Explicit CLI commands continue to work unchanged, and `rpool gui` remains available for compatibility. Global options can still be supplied when launching the default GUI, for example `rpool --rclone C:\Tools\rclone.exe`.

## v0.5.3 dotpush/dotpull portable configuration

rpool can now export the portable part of its configuration as one versioned JSON bundle for dotfiles synchronization:

```text
rpool config export ./managed/rpool/portable-config.json
rpool config import ./managed/rpool/portable-config.json
rpool config import ./managed/rpool/portable-config.json --dry-run
```

The bundle contains named pools, per-remote default paths, and portable GUI defaults. It intentionally excludes rclone credentials, inventory, operation history, and upload/restore journals. The GUI's local `rclone` executable/path is also preserved on import rather than copied from another machine.

This is designed to fit the existing dot workflow: run `config export` immediately before `dotpush`, let the dot system synchronize the JSON file, restore the existing age-encrypted `rclone.conf` during `dotpull`, then run `config import`. For these JSON-only hooks, use the explicit `config export/import` commands above. `examples/dot-rpool-sync.nu` instead implements the crypt-package workflow: capture requires an age recipient, restore requires an identity, and the synchronized tree contains `rpool/config/portable-config.json` plus the encrypted vault when present. Restore the matching rclone configuration first; the package does not provision provider remotes. See [initial setup compatibility](docs/architecture/initial-setup-compatibility.md).

Import validates the complete bundle before making changes. If one of the local config writes then fails, rpool attempts to restore the previous pool, remote-root, and GUI configuration so a partial import is not silently accepted.

## v0.5.2 per-remote default paths

Some rclone backends must be queried or used below a specific path instead of at the remote root. rpool now stores optional per-remote default paths in its own `remote_roots.json` configuration.

```text
rpool remote-root set Instance /data/crypt
rpool remote-root list
rpool remote-root remove Instance
```

With the first setting, a bare `Instance:` resolves to `Instance:/data/crypt` whenever rpool needs a default base. An explicitly supplied path always wins. This is particularly important for path-sensitive capacity backends such as SFTP filesystems mounted below the login root.

The capacity resolver also preserves paths already present in rclone virtual-remote configuration. For example, if a crypt remote wraps `Instance:/data/crypt`, quota lookup is performed against `Instance:/data/crypt` rather than incorrectly collapsing it to `Instance:`. `union`, `crypt`, and `chunker` remain excluded from direct capacity cards.

The GUI exposes the same mapping under **Settings → Per-remote default paths**. The existing global crypt-folder fallback is used only when a crypt remote has no per-remote path override.

This does **not** weaken the encryption rule: `Instance:/data/crypt` may be the physical backing location, but rpool writes archive data only through an encryption-enabled rclone `crypt` remote.

If `/data/crypt` is meant to be the **physical backing root of a crypt remote**, configure that same path in rclone itself, for example `remote = Instance:/data/crypt`. The rpool default-path mapping does not rewrite rclone crypt backend configuration; it controls how bare remote references and capacity/health lookups are resolved.

Example equivalent to a path-sensitive upstream layout:

```text
Instance:/data/crypt
google_1:
drime_1:
dropbox_1:
filen_1:
koofr_1:
box_1:
gms_1:
```

Only `Instance` requires an override; remotes that correctly use their root need no entry.

## v0.5.1 encrypted-storage and capacity fix

- All remote writes are now required to target an rclone `crypt` remote with data encryption enabled (`no_data_encryption` must not be true). Plain provider, union, and chunker destinations are rejected before storage I/O.
- Pool creation also rejects non-crypt destinations, and `rpool doctor` reports legacy pools that contain them.
- The GUI discovers `crypt` remotes for upload/Pool/manifest-replica targets, while capacity cards are built only from physical backing remotes.
- `union`, `crypt`, `chunk`, and `chunker` remotes are excluded from capacity cards. `free-ratio`, status usage, and provider-health quota lookup resolve crypt/chunker targets to their physical backing remote.
- The GUI usage bar is painted from the exact `used / total` ratio across the available pixel width rather than relying on the stock progress widget.

## v0.5.0 continuous integrity

v0.5 adds continuous-integrity and provider-maintenance workflows without changing the archive manifest format.

### Scrub and automatic repair

```text
rpool scrub ./large.iso.rpool.json
rpool scrub ./large.iso.rpool.json --quick
rpool scrub ./large.iso.rpool.json --repair
rpool repair ./large.iso.rpool.json --dry-run
rpool repair ./large.iso.rpool.json --group 3 --group 4
```

A full scrub streams every physical shard and compares BLAKE3 with the manifest. Quick mode checks existence and size only. Reports distinguish missing, wrong-size, corrupt, and transport-error shards and summarize degraded/unrecoverable Reed-Solomon groups.

Repair reconstructs recoverable **data and parity shards**, validates reconstructed BLAKE3 locally, uploads to the original object path, and verifies the archive again. `--group` can be repeated to repair only selected Reed-Solomon groups; selected-group repair still performs a Full BLAKE3 pre-check and post-repair verification for those groups. Plain non-erasure archives can be scrubbed but cannot regenerate missing physical data.

### Provider health

```text
rpool provider health
rpool provider health --pool archive
rpool provider health --json
```

Health checks provider accessibility and latency and appends quota information where `rclone about` is supported. The GUI exposes the same workflow on the **Providers** page.

### Provider drain / migration

```text
rpool provider drain ./large.iso.rpool.json \
  --from old-crypt:rpool \
  --to new-crypt:rpool \
  --dry-run

rpool provider drain ./large.iso.rpool.json \
  --from old-crypt:rpool \
  --to new-crypt:rpool \
  --delete-source
```

Drain follows a copy-first order: copy shards, fully verify the new objects, update the local manifest, replicate the new manifest, then optionally delete the old objects. Source deletion is never implicit. A migration that weakens single-provider failure safety is rejected unless `--allow-risky` is explicitly supplied.

### Stronger resume journals

Interrupted uploads now keep a completed-shard journal alongside the deterministic upload plan. Journal entries are fully checked against remote object size and BLAKE3 before reuse. Restore resume files are versioned and every previously completed output range is re-hashed against the manifest before it is trusted.

### GUI

The GUI now includes:

- **Maintenance → Integrity** — scrub, repair, quick/full mode, dry-run
- **Providers** — health monitoring and drain/migration controls


## v0.4.1 maintenance update

- The Cargo package is now binary-only: `autolib = false`, `autobins = false`, with one explicit `[[bin]]` target named `rpool`.
- The former library entry/dispatch moved to `src/application.rs`; `src/main.rs` owns only crate module registration and process startup.
- Updated the GUI to the eframe/egui 0.36.2 `App::ui` + `Panel` API.
- Normalized the v2 content-root coding marker branch and removed unused public re-exports reported by the compiler.

## What changed in v0.4.0

v0.4.0 adds the first management and maintenance layer on top of the v0.3 storage/GUI core. The complete staged plan lives in [`ROADMAP.md`](ROADMAP.md).

### Named storage pools

A pool stores reusable provider and upload-policy defaults:

```text
rpool pool set archive \
  --remote nas-crypt:rpool \
  --remote gdrive-crypt:rpool \
  --remote sftp-crypt:rpool \
  --data-shards 6 \
  --parity-shards 2 \
  --placement free-ratio

rpool put large.iso --pool archive
```

Useful commands:

```text
rpool pool list
rpool pool show archive
rpool pool remove archive
```

`put --pool` uses the pool's remotes, shard size, worker/retry settings, placement strategy, and Reed-Solomon settings. Explicit put policy flags override pool defaults. `--pool` and explicit `--remote` are intentionally mutually exclusive.

The GUI Upload page also discovers configured pools at startup. Selecting a pool delegates destination and policy selection to the pool instead of the GUI's manual destination settings. The **Pools** page can create, load, edit, and remove pool definitions without duplicating the pool validation/storage logic.

### Manifest replica management

Uploads still write a complete `manifest.json` replica to each storage destination. v0.4 makes those replicas observable and repairable:

```text
rpool manifest verify ./large.iso.rpool.json
rpool manifest replicate ./large.iso.rpool.json
rpool manifest recover <archive-id> --pool archive
```

`manifest verify` validates every selected replica and compares its semantic manifest fingerprint with the reference. `manifest replicate` repairs or adds copies. `manifest recover` searches providers until it finds a valid replica and writes a local `.rpool.json`. The GUI exposes the same workflows on the **Manifest replicas** page.

### Rebuildable inventory

The local inventory is a convenience index, **not** the source of truth. It can be deleted and rebuilt from manifests:

```text
rpool inventory add ./large.iso.rpool.json
rpool inventory rebuild ./manifests
rpool inventory list
rpool inventory find '*.iso'
rpool inventory info <archive-id>
```

Successful uploads and manifest recovery refresh the inventory automatically when possible. Inventory update failure does not invalidate an otherwise completed archive.

### Task history

CLI operations are recorded as compact JSONL records containing operation id/type, target, timestamps, status, and a bounded error summary. Raw command lines, rclone credentials, and tokens are not intentionally persisted. GUI storage actions are covered automatically because the GUI executes the same CLI path.

```text
rpool history list --limit 50
rpool history prune --keep 500
```

### Doctor

```text
rpool doctor
rpool doctor --json
```

Doctor checks the rclone executable/version, discoverable remotes, pool validity, pool references to rclone remotes, shared configuration paths, inventory readability, and task-history readability. The GUI exposes Doctor under **Maintenance → Diagnostics**, metadata rebuild tools under **Maintenance → Metadata**, and history list/pruning under **Jobs**.

## What changed in v0.3.0

v0.3.0 adds a native GUI control console while preserving the existing CLI and manifest format.

- `rpool gui` launches the native storage console
- **Dashboard** shows per-provider used/free/total space with progress bars and an aggregate known-capacity summary
- **Files** groups Inventory, Upload, Restore, Verify, and Status behind local tabs
- **Storage** groups Providers and Pools
- **Jobs** exposes running progress, the upload queue, local history, expandable command/log details, recorded failure messages, and safe in-session retry for reconstructable failed/cancelled work
- **Maintenance** groups Integrity, Metadata, and Diagnostics
- **Settings** retains GUI and operation defaults
- long-running operations run outside the GUI thread and stream stdout/stderr into a persistent operation console
- running operations can be cancelled from the GUI
- GUI defaults can be saved independently of archive manifests
- discovered rclone remotes can be added as upload targets with a configurable default remote folder
- GUI code follows the same feature ownership rules under `src/gui/screens/` and reusable UI components under `src/gui/widgets/`
- the GUI calls the same rpool CLI implementation for storage operations rather than duplicating put/get/verify/status behavior

### Source layout

```text
src/
├─ main.rs                 # binary entry point only
├─ application.rs          # CLI dispatch + task recording
├─ cli.rs                  # command-line schema
├─ commands/               # put/get/verify/status/usage/config-sync orchestration
├─ config_sync/            # portable dotpush/dotpull configuration bundles
├─ doctor/                 # maintenance diagnostics
├─ journal/                # upload/restore resume state validation
├─ maintenance/            # scrub, recoverability, automatic repair
├─ erasure/                # Reed-Solomon encode/reconstruct/validation
├─ gui/
│  ├─ app.rs               # top-level native GUI composition
│  ├─ settings.rs          # persistent GUI defaults
│  ├─ state/               # GUI state, selection, and startup persistence
│  ├─ task_runner.rs       # background CLI process + log/cancel handling
│  ├─ usage_refresh.rs     # non-blocking cloud quota refresh
│  ├─ screens/             # work-area folders and one file per owned GUI feature
│  └─ widgets/             # reusable GUI controls
├─ history/                # append-only operation history
├─ inventory/              # rebuildable manifest-derived archive catalog
├─ manifest/               # manifest loading, replication, recovery, validation
├─ models/                 # serializable/domain data structures
├─ placement/              # provider placement strategies
├─ pool/                   # persistent pool loading/validation/resolution
├─ provider/               # health monitoring and migration policy
├─ remote_root/            # per-remote default/base path configuration
├─ planning/               # upload plans and failure-domain analysis
├─ presentation/           # human-readable formatting/tables
├─ storage/                # rclone transfer, quota, stat, verification
└─ utils/                  # reusable low-level helpers
```

## GUI

The native console is now the default when no CLI subcommand is supplied:

```text
rpool
```

The explicit form remains supported:

```text
rpool gui
```

Or from Nushell after sourcing `rpool.nu`:

```text
rgui
```

The GUI starts by querying configured remotes. **Dashboard** shows physical-provider capacity information; **Files → Upload** can reuse discovered safe crypt remotes as upload destinations and append the configured default folder (initially `rpool`).

GUI settings are stored separately from archive manifests:

- Windows: `%APPDATA%\rpool\gui.json`
- macOS: `~/Library/Application Support/rpool/gui.json`
- Linux: `$XDG_CONFIG_HOME/rpool/gui.json` or `~/.config/rpool/gui.json`

The GUI uses `eframe/egui` for the native interface and `rfd` for native file dialogs.

## What changed in v0.2.0

- Reed-Solomon GF(256) erasure coding: configurable `K data + M parity`
- automatic recovery during `get` when up to `M` real data shards are unavailable in a coding group
- parity is generated in small stripes instead of loading an entire 220 MiB × `(K+M)` group into RAM
- last incomplete coding group is zero-padded internally; the original logical file length is preserved
- data and parity placement is spread across different remotes when possible
- `free-ratio` placement now accounts for parity capacity as well as data capacity
- `status` reports coding parameters, degraded groups, and unrecoverable groups
- new `usage` command displays used/free/total space for each cloud remote
- `status --usage` appends the same quota table to an archive status report
- v0.1 manifest reading is retained for restore/verify/status compatibility

The default remains **plain striping with no parity** so upgrading from v0.1 does not silently consume additional cloud storage. Enable Reed-Solomon explicitly with `--parity-shards`.

## Build

```text
cargo build --release
```

Binary:

```text
target/release/rpool
```

`Cargo.toml` uses:

```text
reed-solomon-erasure = "6"
eframe = "0.36.2"
rfd = "0.17.2"
```

The GUI dependency set requires a current Rust toolchain; egui 0.36.2 requires Rust 1.95 or later.

Put the resulting `rpool` binary somewhere in `PATH` together with `rclone`.

## Recommended rclone layout

Create one `crypt` remote per provider, for example:

```text
gdrive-crypt:
mega-crypt:
box-crypt:
koofr-crypt:
```

Each `crypt` remote can point at its provider-specific storage directory.

Do not place rclone `chunker` between `rpool` and these remotes. `rpool` already performs logical sharding and, when enabled, parity generation.

## Plain upload: v0.1-compatible behavior

```text
rpool put large.iso \
  --remote gdrive-crypt:rpool \
  --remote mega-crypt:rpool \
  --remote box-crypt:rpool \
  --remote koofr-crypt:rpool \
  --workers 8
```

Default data shard size is 220 MiB. Default placement is deterministic round-robin.

## Reed-Solomon upload

Choose `K+M` with both storage overhead and provider failure domains in mind. With the four-provider example below, `6+2` spreads eight physical shards as two per provider under balanced placement, so loss of one complete provider is within the two-shard parity budget:

```text
rpool put large.iso \
  --remote gdrive-crypt:rpool \
  --remote mega-crypt:rpool \
  --remote box-crypt:rpool \
  --remote koofr-crypt:rpool \
  --data-shards 6 \
  --parity-shards 2 \
  --workers 8
```

For each coding group, this produces up to six real data shards plus two parity shards. Any two unavailable real data shards in that group can be reconstructed as long as enough other data/parity shards remain valid.

`8+2` remains a useful 25% overhead layout when at least five independent providers are available and placement stays balanced at no more than two physical shards from a group per provider.

Storage overhead is approximately:

```text
parity / data = M / K
```

Examples:

| Layout | Approx. overhead | Max missing shards recoverable per group | Min. balanced providers for whole-provider loss* |
|---|---:|---:|---:|
| 6+2 | 33.3% | 2 | 4 |
| 8+1 | 12.5% | 1 | 9 |
| 8+2 | 25% | 2 | 5 |
| 10+2 | 20% | 2 | 6 |
| 12+3 | 25% | 3 | 5 |

\* This assumes physical shards in each group are distributed as evenly as possible and one provider is the failure domain. rpool reports the actual result rather than assuming the layout is safe.

The GF(256) backend requires `K + M <= 255`.

### Placement and provider failure

With round-robin placement, the physical shards of each coding group are emitted group-by-group and distributed evenly across the configured remotes.

With `--placement free-ratio`, rpool queries `rclone about --json`, accounts for both data and parity bytes, and first minimizes the number of same-group shards already assigned to each provider before using remaining-free-space ratio as the tie-breaker.

```text
rpool put large.iso \
  --remote gdrive-crypt:rpool \
  --remote mega-crypt:rpool \
  --remote box-crypt:rpool \
  --remote koofr-crypt:rpool \
  --data-shards 6 \
  --parity-shards 2 \
  --placement free-ratio
```

A coding layout cannot protect against a provider outage if too many shards from the same group are stored on that provider. During `put`, rpool prints a warning when the actual plan places more than `M` shards from any coding group on one provider. `status` also reports `single_provider_failure_safe=true/false`, the maximum same-group shard count on one provider, and the parity budget.

## Restore

```text
rpool get large.iso.rpool.json restored.iso --workers 8
```

Or retrieve a replicated manifest from any surviving remote:

```text
rpool get "gdrive-crypt:rpool/<archive-id>/manifest.json" restored.iso --workers 8
```

For a Reed-Solomon manifest, restore behavior is:

1. Try each required data shard normally.
2. BLAKE3-validate every downloaded data shard.
3. If a data shard is missing or corrupt, fetch only as many valid parity shards as needed.
4. Reconstruct the missing data in small stripes.
5. BLAKE3-validate every reconstructed shard before declaring it complete.

This avoids downloading parity during a healthy restore.

## Verify

Fast physical existence/size verification:

```text
rpool verify large.iso.rpool.json
```

Full remote read + BLAKE3 verification of both data and parity:

```text
rpool verify large.iso.rpool.json --full --workers 8
```

`verify` is intentionally strict: it reports missing parity as a verification failure even when the archive is still recoverable. Use `status` to see the actual recoverability state.

## Status and recoverability

```text
rpool status large.iso.rpool.json
```

For Reed-Solomon archives the output includes values similar to:

```text
coding=reed-solomon-gf256 data=6 parity=2 overhead=33.3% groups=12
recoverable_groups=12/12 degraded_groups=1 unrecoverable_groups=0
single_provider_failure_safe=true max_group_shards_on_one_provider=2 parity_budget=2
```

It also groups physical shard health by remote.

Append cloud quota information:

```text
rpool status large.iso.rpool.json --usage
```

## Cloud usage / capacity table

With no arguments, query every remote returned by `rclone listremotes`:

```text
rpool usage
```

Or query explicit remotes:

```text
rpool usage \
  --remote gdrive-crypt:rpool \
  --remote mega-crypt:rpool \
  --remote box-crypt:rpool \
  --remote koofr-crypt:rpool
```

Example shape:

```text
REMOTE                 USED        FREE       TOTAL    USED%  USAGE              TRASH  STATUS
----------------  ----------  ----------  ----------  -------  ------------  ----------  ------------------
gdrive:              8.40 GiB    6.60 GiB   15.00 GiB    56.0%  [######----]      0.00 B  ok
mega:               11.20 GiB    8.80 GiB   20.00 GiB    56.0%  [######----]         n/a  ok
```

The exact fields depend on what the physical provider exposes through `rclone about --json`. `union`, `crypt`, and `chunker` layers are not shown as capacity providers. Unsupported values are shown as `n/a` instead of being guessed.

You can derive all remotes from a manifest:

```text
rpool usage --manifest large.iso.rpool.json
```

Machine-readable output:

```text
rpool usage --manifest large.iso.rpool.json --json
```

## Nushell wrappers

Source `rpool.nu` from your Nushell configuration. It exports:

```text
rpush
rpull
rverify
rstatus
rusage
rgui
rpools
rmanifest
rinventory
rhistory
rdoctor
rscrub
rrepair
rprovider
rroots
rconfig
```

Example:

```text
rpush large.iso \
  --remote gdrive-crypt:rpool \
  --remote mega-crypt:rpool \
  --remote box-crypt:rpool \
  --data-shards 6 \
  --parity-shards 2

rstatus large.iso.rpool.json --usage
rpull large.iso.rpool.json restored.iso
```

## Box note

The default `220 MiB` value is the plaintext **data-shard** size. When a data shard is uploaded through an rclone `crypt` remote, crypt framing increases its underlying provider object size. 220 MiB leaves headroom below Box Free's 250 MB single-file limit.

Parity objects use the same plaintext `shard_size`, so the same provider object-size consideration applies to parity.

## Resume behavior

Upload creates:

```text
<source>.rpool.upload.json
<source>.rpool.upload.state.json
```

before/during transfers. The plan freezes data/parity placement so `free-ratio` does not produce a different layout after interruption. The state journal records completed physical shards. On resume, recorded shards are streamed and BLAKE3-validated before they are trusted; stale/corrupt journal entries are discarded and uploaded again.

After a successful upload the plan file is removed and the permanent manifest remains as:

```text
<source>.rpool.json
```

Download creates:

```text
<output>.rpool.resume.json
```

Only BLAKE3-validated data shards are marked complete. A reconstructed shard is also BLAKE3-validated before being added to resume state.

## Erasure-coding implementation notes

- Algorithm: Reed-Solomon over GF(256)
- Encoding/decoding stripe size: 4 MiB by default in the manifest
- Data shard size: configurable with `--shard-mib`
- Parity shard size: one full plaintext shard
- Final partial data shard: zero-padded only for parity mathematics; its stored data object and restored logical length stay unpadded
- Final incomplete coding group: non-existent data slots are deterministic all-zero virtual shards and are not uploaded
- Parity generation uses temporary local parity files one coding group at a time; they are removed after successful transfer
- Healthy restore does not download parity

## Compatibility

v0.5 reads legacy v0.1 manifests for the compatible restore/verify/status paths and continues to write manifest version 2. v0.5 integrity and provider-maintenance features operate on the existing manifest metadata and do not require an archive-format migration.

An interrupted v0.1 upload plan without erasure coding can also be resumed because the added upload-plan fields have backward-compatible defaults.

## Current limitations

- Automatic repair restores recoverable shards to their manifest-declared provider/object paths. If an entire provider is permanently unavailable, use provider migration/drain while the source is still readable; direct repair-to-a-replacement-provider is not yet a single combined operation.
- Provider failure domains are currently identified by configured remote-base strings. Two different rclone remotes or paths backed by the same physical/cloud account are not automatically recognized as the same failure domain.
- `rclone about` support varies by backend. The `usage` command reports `n/a` for values a provider does not expose.
- Reed-Solomon protects shards inside each coding group. It does not replace backups, account recovery, or independent copies of the manifest.


### Integrity overview

Maintenance > Integrity keeps the most recent scrub/repair summary locally so the GUI can show the last known shard health after restart. The summary is advisory metadata; manifests and remote shards remain the source of truth.

The Scrub workflow can target either an indexed Library file or a manifest path directly. Quick mode checks object existence and size, while Full BLAKE3 streams every shard for content verification. GUI-launched scrubs report shard-count progress through the structured progress channel, and the last check retains non-healthy shard details plus per-group recoverability after restart.

Repair is linked to the last Scrub result for the currently selected target. Recoverable groups can be selected individually; provider-error and unrecoverable groups are shown separately and cannot be selected. Repair always performs a Full BLAKE3 pre-check before reconstruction, even if the preceding Scrub was Quick.


### Metadata and diagnostics

Maintenance > Metadata groups manifest replica verification/repair/recovery with inventory rebuild. The inventory remains a rebuildable local cache; manifests and remote shards remain authoritative. Maintenance > Diagnostics runs Doctor checks and presents each check as OK, WARN, FAIL, or INFO, including rclone, remotes, pools, local inventory/history, and the last integrity snapshot.
