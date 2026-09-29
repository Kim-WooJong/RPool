# Changelog

## Unreleased

- Native Linux FUSE frontend (opt-in): `rpool mount --virtual-drive --frontend
  fuse [--native-read-only]` mounts a local virtual-drive workspace through the
  filesystem core, with no rclone mount or WebDAV loopback. `close`/`fsync` are
  the local durability points; sync stays asynchronous. Pool-sync and shared
  workspaces keep the DAV frontend, which remains the default.

- Internal: `src/mount/fs_core`, a protocol-independent filesystem core with
  handle APIs over the virtual drive (native mount milestone M3a). `fsync`
  is the local durability acknowledgement; upload never gates it. No frontend
  uses it yet; DAV directory create/remove now share the drive's rules.
  M3b adds test-only crash points, a crash matrix, randomized model traces
  and a stalled-uploader test; they fixed `rename(x, x)` of a missing file and
  sibling conflicts for files recreated after a pending delete.

- `rpool config paths` prints the active settings file paths as JSON (no file
  contents). Portable config bundles now carry the GUI encryption preferences
  (entropy bits, filename/directory encryption, never passwords); import
  validates them, and bundles without them keep the local preferences.
  `scripts/portable-config-path-test.nu` checks this against the real CLI in
  an isolated home.

- Opt-in native crypt writes per pool (`pool set --native-crypt`, GUI
  "Encrypt in RPool"). `put --pool` and reprocess encrypt shards in RPool in
  rclone crypt format and upload them to the crypt remote's base; readback
  still uses rclone crypt. Unsupported crypt options, wrapping bases and
  `RCLONE_CRYPT_*`/`RCLONE_CONFIG_*` overrides are refused. Mounts and other
  commands keep using rclone crypt. Pools without the flag are unchanged.
  Milestone M2 of `docs/NATIVE_MOUNT_CRYPT_PLAN.md`.

- Internal: `src/crypt`, an rclone-crypt-compatible format library (keys,
  obscure/reveal, streaming and ranged data, standard/obfuscate/off names).
  It is milestone M1 of `docs/NATIVE_MOUNT_CRYPT_PLAN.md`, verified two-way
  against rclone on local directories. Nothing uses it yet; behaviour is
  unchanged.

- Safer native mount shutdown. Before quitting rclone, RPool now asks rclone to
  upload its delayed (60 s) write-back queue into the still-running WebDAV
  backend and waits up to 90 s for it to empty. rclone writes its output to a
  private `.rpool/rclone-mount.log` (the previous session is kept as
  `rclone-mount.previous.log`) instead of pipes, so it cannot be killed by
  SIGPIPE if RPool exits first. When a macOS NFS stop is uncertain, the WebDAV
  backend now stays up until rclone exits instead of disappearing under a live
  kernel mount.

- Default data shard size is now 64 MiB (was 220 MiB). Saved pools keep their
  stored `shard_mib`; existing manifests are unaffected. Shard sizes are
  limited to 1–4096 MiB in CLI, GUI and pool validation, and MiB→bytes
  conversion is centralized. The mount capacity search now simulates up to
  262,144 physical shards, so multi-TiB pools are not under-reported with the
  smaller shards.

- Pools accept an optional `max_object_bytes` provider object limit
  (`pool set --max-object-bytes`, GUI "Provider object limit"). Encrypted
  (rclone crypt) shard object size is checked before upload; coded manifests
  reject data shards larger than `shard_size`.

- Pool capacity now foregrounds summed independent account storage after
  data/parity overhead. Unverified backing accounts get a separately labelled
  independence what-if estimate; actual admission/OS free space remains
  conservative. Resilient partial stripes no longer show a false zero upper
  bound merely because a full stripe needs more outage groups.

- Pool provider selection now adds discovered crypt remotes at `name:` without
  appending the GUI default folder. Existing saved destinations are unchanged.

- Provider health now probes encrypted remote roots rather than appending Pool
  storage prefixes, preventing false missing-directory failures. A Windows-only
  unused-argument build warning in pool transition durability handling is fixed.

- Added selectable `capacity-first` placement for heterogeneous account quotas.
  It prioritizes the largest remaining account budget without Resilient's
  provider-outage bound; parity remains but a provider outage may be unrecoverable.
  Existing `resilient` placement and saved defaults retain their behavior.

- Documented the phased online-write/native-frontend plan and added aggregate
  DAV write diagnostics. Content-Range now validates received body length before
  sealing; local tests distinguish one fragmented write from repeated full PUTs.

- Per-pool machine-local mount profiles restore workspace, automatic history deletion, cache and other options on pool selection. Save explicitly or on launch; legacy global cache settings remain safe defaults.
- Existing online workspaces use validated current upload policy for compatible configuration edits. Explicit source-preserving Apply pool changes stages a fresh metadata generation for changed membership and keeps the selected pool/workspace path. Current known files and sealed writes are verified before activation; historical/native recovery data stay in a retained backup, not silently discarded or claimed migrated.

- Online mount startup separates required metadata refresh from pending uploads/GC and moves capacity reporting off the control loop. Online unmount cancels supervised remote work and retains pending local data instead of forcing a final full cloud sync. Explicit sync remains available; legacy bounded coordinator bootstrap and replica mode retain their existing behavior.
- Mount adapter releases its OS lease explicitly on drop, preventing an inherited file descriptor from delaying immediate remount; durable surviving-process fences remain enforced.

- Completed Reprocess plans can supply exact verified replacement archives to recovery without duplicate uploads; full mounted paths are retained. Explorer no longer receives unsupported quota properties when capacity is stale/missing: the DAV bridge reports known usage and zero verified additional free space until a fresh sample is available, avoiding rclone's synthetic 1 PiB fallback.

- Explicit account-failure recovery to a differently named writable pool/workspace on remaining accounts, preserving the source. GUI/CLI expose verified-copy recovery and explicit read exclusions; unresolved data remains reported. Ordinary membership/retention guards remain in place. Mount startup stages are logged before cloud synchronization so a running process is not mistaken for a mounted drive.

- GUI mounts default to online/on-demand files with automatic pool metadata sync; explicit replica mode and legacy workspace guards remain. Cache limits and online/replica preference persist locally via Save or mount/sync launch. Cleanup reporting includes bytes reclaimed on cache startup. Existing replica files and uncertain native recovery data are never silently migrated or deleted.

- Configurable native mount cache size/free-disk targets, GUI pending-spool budget, and access-based clean-shard LRU with pre-download admission. Dirty/open native data stays protected; native limits are soft and replica files/recovery data are not cache-evicted.

- Automatic pool sync (`--virtual-drive --pool-sync`): immutable metadata replicated inside existing encrypted pool destinations, no shared-root setup or coordinator PC. New namespace/workspace v6 preserves original + every worker-labelled concurrent edit and exposes structured GUI conflict groups. Publication is acknowledged only after all configured metadata replicas verify it. Per-workspace `history_limit` config is saved for future retention, **not enforced**; append-only peer history/bootstrap limits remain. Revision-changing reads require remount to fence unconditioned native range requests. No automatic legacy migration or real-cloud/native validation.
- Cloud-authoritative shared drives now require successful startup synchronization before mounting; GUI/CLI default to zero previous revisions. File contents remain lazy downloads; stale edits remain local recovery data.
- Opt-in bounded shared virtual-v5 history, GUI/CLI: one designated coordinator activates complete checkpoints, retains latest + configurable previous versions (default 0: latest only), and journals exact owned-object reclamation. Offline clients no longer pin historical cloud data; stale unsynced operations are isolated locally. Publication is not acknowledgement. Attempt-ledger sweeps track interrupted/late uploads, and durable high-water checks reject rollback before GC. NEW workspaces required; legacy/imported archives are not auto-deleted. Prior native cache is isolated intact on bounded restart. Real multi-PC/native deployment remains unverified.

- Opt-in metadata-first virtual drive via authenticated persistent loopback DAV: lazy verified shard reads, group-local RS recovery, bounded clean cache, durable write spool, live causal metadata exchange and worker-labelled conflict copies. Incoming replacements of already served paths appear as copies until remount; native handle-coherent replacement and real WinFsp/FUSE deployment remain unvalidated.
- Provider quotas are queried dynamically. Non-secret account/outage declarations separate overlapping budgets from correlated failures; capacity/admission use the selected placement and exact per-domain budgets. Virtual usage excludes parity/cache and is exposed through DAV quota. Explicit spool export and clean-cache trimming retain remote history; bounded-v5 retention is a separate opt-in policy. No automatic corrupt-checkpoint rollback.

- Quota-aware mount writeback excludes unknown/unavailable accounting targets without changing Pool policy or old reads. Mount panel displays exclusions, local logical usage and conservative parity-aware capacity estimates. Explicit unmounted migration copies and verifies affected active archives, switches references with durable receipts and publishes manifest-only shared successors; originals/history remain stored. OS filesystem capacity remains local-disk-based.

- Delayed bounded hedged RS downloads: passive stall/progress estimates, verified per-read staging, early per-group recovery and cooperative cancellation of losing requests. Two-group staging window and reserved parity slots remain within the worker cap. Superseded slow candidates are retained for required recovery; parent cancellation/deadlines and fatal adapter errors remain visible. Single-worker reads retain non-speculative recovery. No real-cloud latency claim.

- Fair bounded upload/download dispatcher, cross-group direct restores, worker-free retry backoff, overlapping bounded parity generation/uploads and immediate per-shard checkpoints. Restore-only parity fallback now includes exhausted timeout/transient/rate-limit errors; local-output and auth/cancel failures remain terminal, and acknowledged writes are not replayed after readback failure.
- Optional Resilient placement resolves known backing aliases and rejects per-group target concentration above the parity budget. Compatibility round-robin balances configured names; free-ratio no longer silently exceeds its balanced group ceiling. Existing Pool policies are unchanged. Live multi-cloud speed/outage validation remains outstanding.

- Optional shared workspace namespace with encrypted immutable revision exchange, causal offline-edit detection, worker-named conflict copies, shared logical deletion and edit/delete preservation. GUI/CLI accept a shared root and worker name. Incoming filesystem changes are applied only during cache-empty unmounted reconciliation; mounted mode exchanges revisions without replacing files. Catalog v2 protects shared ancestry from older binaries; interrupted applies retain recovery copies.

- Read/write Pool drive via a persistent local workspace and rclone/WinFsp/FUSE. Independent GUI mount task, explicit verified imports, atomic local catalog, verified background archive versions, retained deletion history and restart-safe cache handling. Full local disk space is required; no on-demand/distributed namespace claim.

- Reprocess supports provider/policy drafts, changed-layout summaries, verified-copy readiness estimates, persisted plans and safe per-item resume. Original archives remain intact; completed replacements are revalidated before reuse.
- Reprocess completion receipts and inventory replacement are atomic; OS locks prevent duplicate plan execution and concurrent inventory-add races.

- New crypt remotes now point directly at the provider's configured remote default path: `/data` becomes `provider:/data`, without an encryption parent or unique child directory. Existing crypt paths and keys are unchanged.
- Removed the redundant encryption parent-folder controls. Historical `root` settings/CLI arguments remain accepted but ignored. Version stays 0.6.0 until the next batched update.

## 0.6.0 — 2026-09-28

- Bundled provider connection and automatic missing-crypt provisioning, with existing keys preserved.
- Added Settings encryption defaults (1024-bit password entropy by default), configuration status indicators and automatic refresh after the connection wizard exits.
- Added encrypted-provider pool picker and explicit-source, copy-only reprocessing.
- Fixed crypt provisioning to resolve the provider's remote default path before adding the relative encryption parent and unique child folder.
- Fixed Windows-only unused-mut warning in reprocessing directory creation.
- Source changes are managed directly in Git; versions are batched according to docs/VERSIONING.md. No archive-format migration or existing crypt relocation.
- Validation: macOS default 189 passed and optional OpenDAL 200 passed (12 ignored in each), release build passed with warnings denied. Windows/Linux GUI and live-cloud execution were not validated for this batch.

### Storage implementation carried into this batch

- Introduced storage contracts, synthetic Memory/Unix Local adapters and optional test-only OpenDAL Memory.
- Unified rclone execution, verified read/write/repair/migration and admin/diagnostic boundaries.
- Preserved crypt portability and legacy root domains; explicit manifest replication/recovery now retains source bytes.
- Removed retired bridges and unused historical files using an explicit cleanup list.
- Validation and limitations: docs/architecture/verification.md. No release readiness or Windows/Linux execution claim.
- The former 0.5.16 reservation is superseded by the batched semantic-version policy.

## 0.5.15

### Crypt Secret Portability — B1-B7

- Added a strict schema-v1 `SecretBundle` that stores only rclone-obscured `crypt` `password` / optional `password2` values; no deobscure API is provided.
- Added in-memory `rclone config dump` extraction that ignores credentials from non-crypt remotes and never writes the full dump to disk.
- Added streaming age encryption to `secrets/rclone.age` with no plaintext secret temp file and no exported private identity.
- Added exact `--no-obscure` restore with schema/name/type/structure validation and byte-for-byte obscured-value verification.
- Added format-aware transaction/rollback handling for encrypted and plaintext rclone configs, including encrypted recovery snapshots and fail-closed interrupted recovery.
- Added ignored real-tool B6 tests for local+crypt sentinel recovery, wrong/missing age identity, corrupted vaults, optional password2, multi-remote behavior, and idempotency.
- Added top-level `rpool export` / `rpool import` orchestration. The artifact binds `config/portable-config.json` to `secrets/rclone.age` by BLAKE3 digest so mixed exports are rejected before mutation.
- New crypt-secret generation uses independent 1024-bit OS randomness for each remote's `password` and `password2`; existing crypt keys are never auto-rotated or backfilled.
- Retained legacy `rpool config export/import` as JSON-only portable-settings commands that intentionally refuse crypt-aware bundles.

### Maintenance UI — C1

- Added a persistent integrity summary written by scrub and repair operations.
- Maintenance > Integrity now shows the last recorded healthy, missing, corrupt, recoverable, and unrecoverable counts.
- Integrity summaries survive GUI restarts and refresh automatically when a scrub or repair task finishes.
- Split the Integrity screen into state, summary, and action modules in preparation for the remaining Maintenance redesign steps.

### Maintenance UI — C2

- Rebuilt Scrub as a dedicated workflow with Library-file or direct-manifest target selection.
- Added explicit Quick and Full BLAKE3 modes plus a pre-run summary describing target, coding, original size, and worker count when known.
- Added shard-count structured progress events so scrub progress is shown independently from raw rclone logs.
- Persist integrity snapshots now retain non-healthy shard details for restart-safe result inspection.
- Added a last-check issue table listing shard, status, coding group, remote, and diagnostic detail.
- Kept Repair available as a compatibility control pending the dedicated C3 workflow.

### Maintenance UI — C3

- Linked Repair directly to the most recent Scrub result and require the selected target to match that snapshot.
- Added persisted per-group recoverability analysis so provider-error, recoverable, and unrecoverable groups remain distinct after restart.
- Added group selection plus select-all/clear controls; only proven recoverable groups can be selected.
- Added `rpool repair --group <N>` for safe repeated group-scoped repair.
- Selected-group repair performs a Full BLAKE3 pre-check even when the previous Scrub was Quick, and verifies the selected groups again after reconstruction.
- Provider/transport errors block only affected selected groups instead of being silently treated as erasures.
- Added item-count progress for the reconstruction phase and reset item progress cleanly between scan/repair phases.
- Removed the legacy Repair control in favor of the scrub-linked workflow.


### Maintenance UI — C4

- Consolidated Maintenance into Integrity, Metadata, and Diagnostics work areas.
- Moved manifest replica verification/repair/recovery and inventory rebuild into a single Metadata screen.
- Reworked Doctor into an in-GUI diagnostics table with OK/WARN/FAIL/INFO status presentation.
- Added integrity-snapshot readability to Doctor so damaged local maintenance metadata is surfaced explicitly.
- Kept inventory and integrity snapshots rebuildable/advisory; manifests and remote shards remain the source of truth.
- Added obsolete Maintenance single-file modules to the upgrade cleanup list.


## v0.5.14 (in development)

### GUI redesign — Storage

- B1: Rebuilt Storage > Providers around a single physical-provider table with capacity, used/free values, reachability, and latency.
- Reused the existing asynchronous usage refresh to collect provider health and quota in one pass instead of launching a separate child command.
- Kept virtual rclone layers (union/crypt/chunk/chunker) out of the capacity list; rows represent resolved physical capacity targets.
- Moved provider migration controls behind a collapsed tools section while preserving the existing drain command path.
- Migrated the Providers screen to a folder module in preparation for the provider-details step and added the retired single-file module to upgrade cleanup.
- Added provider selection and a details pane showing the physical backing path, configured default root, used/free/total capacity, latency, health errors, and linked crypt remotes.
- Extended the background provider refresh with crypt-to-physical backing mappings so the details pane does not run rclone discovery on the UI thread.
- On Windows, default GUI launches and explicit `rpool gui` invocations relaunch as a `CREATE_NO_WINDOW` child, while CLI commands retain normal console behavior.
- Rebuilt Pools as a list/details workflow showing crypt targets, unique failure domains, Reed-Solomon policy, placement, raw backing capacity, estimated usable free space, and single-provider failure-safety status.
- Counted multiple crypt remotes that resolve to the same physical backing as one failure domain for pool safety analysis.
- Replaced the direct provider drain button with a four-stage migration workflow: Preview, backend validation, explicit confirmation, then execution.
- Migration preview runs the real `provider drain --dry-run` path, captures the exact shard move plan, and is invalidated whenever migration inputs change.
- Risk-reducing overrides and source deletion now require a validated preview plus explicit confirmation; source deletion remains last after copy, full verification, manifest update, and replica write.

## v0.5.13 (in development)

### GUI redesign — Jobs / Progress

- Unified task and upload queue states under the shared `JobStatus` model.
- Split task metadata/progress from raw stdout/stderr logs.
- Added a structured internal progress protocol enabled only for GUI-launched child operations.
- Added logical completed bytes, actual network bytes, transfer rate, ETA, and batch position to `TaskProgress`.
- Added upload progress for data/parity work and restore progress for direct/reconstructed shards.
- Reworked the bottom operation panel so progress is primary and command/raw logs live under collapsed Details.
- Added a reusable exact-width task progress widget.
- Fixed the Nushell cleanup message syntax that previously treated `file(s)` as interpolation.
- Rebuilt Jobs into separate Running, Queue, and History sections.
- Integrated the upload batch queue with the shared `JobStatus` presentation.
- Switched GUI history viewing/pruning to direct local history access so it does not consume the single child-operation slot.
- Added automatic Jobs/Dashboard history refresh when GUI-launched operations finish.
- Added reusable job-status badges plus shared operation-name, truncation, and duration formatting.
- Migrated Jobs to a folder module and added the retired `src/gui/screens/jobs.rs` path to upgrade cleanup.
- Added expandable current-task details with the exact command preview plus raw stdout/stderr/system output.
- Added selectable History details with recorded failure messages, timing, target, and job ID.
- Added safe in-session Retry for failed/cancelled jobs by retaining the exact original GUI invocation in memory.
- Integrated upload retry with the existing batch queue so a failed/cancelled upload item is returned to the queue and the batch can continue after a successful retry.
- Deliberately do not reconstruct executable commands from persisted history records; stored history remains descriptive unless the original GUI-session invocation is still available.

## v0.5.12

### Build hotfix and upgrade cleanup

- Explicitly typed GUI pool-name collection as `Vec<String>` to prevent slice inference errors during `collect()`.
- Updated egui 0.36.2 drag-and-drop handling from the removed `DroppedFile.path` field access to `DroppedFile::path().to_path_buf()`.
- Added `scripts/update-cleanup.nu` with `--dry-run` support for removing obsolete GUI source files left behind by in-place upgrades.
- Kept the existing explicit folder-module paths as a second layer of protection against stale source files.
- No storage, crypt, manifest, or upload-policy behavior changed.

## v0.5.11

### GUI module-layout hotfix

- Fixed Rust `E0761` module ambiguity when older single-file GUI modules remain beside newer folder-based modules after an in-place ZIP update.
- Pinned the migrated `state`, `dashboard`, `maintenance`, `inventory`, and `upload` modules to their explicit `*/mod.rs` paths.
- This keeps in-place upgrades compatible even when stale legacy `.rs` files are still present.
- Removed the unused unsized `capacity_bar` wrapper; the dashboard continues to use the shared sized capacity bar.
- No storage, crypt, manifest, upload, or GUI workflow behavior was changed.

## v0.5.10

### GUI redesign — patch 6

- Reworked Files > Upload around storage pools as the default destination model.
- Added multi-file selection and desktop drag-and-drop with a visible per-file queue.
- Added sequential batch execution through the existing `rpool put` path; failures/cancellations pause the batch while keeping remaining files pending.
- Moved manual one-off destinations plus shard/worker/retry/placement/RS controls under a collapsed Advanced section.
- Kept custom Archive ID available only for single-file uploads; multi-file uploads generate IDs automatically.
- Added a compact upload preflight summary for files, target, coding policy, and encrypted destination count.
- Added shared preflight validation that blocks upload for missing files, invalid pool/manual policy, invalid RS parameters, or known non-crypt destinations.
- Kept low-level crypt enforcement authoritative when GUI crypt discovery is temporarily unavailable.
- Cached pool definitions in GUI state so the upload summary does not reread pool configuration every frame.
- Kept archive format, CLI storage behavior, manifest v2, and crypt write enforcement unchanged.

## v0.5.9

### GUI redesign — patch 5

- Rebuilt Files > Library as the primary inventory-backed file browser.
- Added search across file names, archive IDs, manifest locations, pool labels, and storage remotes.
- Added pool and erasure-coding filters plus sortable Name / Size / Pool / Coding / Created columns.
- Added file selection with a dedicated details pane for archive ID, manifest source, coding, remotes, size, and inferred pool.
- Added direct Restore / Verify / Status transitions that prefill the selected manifest and reuse the existing command workflows.
- Kept live health explicit: inventory entries show `Not checked` because the rebuildable inventory cache does not store current remote health.
- Pool labels are shown only when resolved remotes and coding parameters match a configured pool; ambiguous matches are labeled `Multiple`.
- Preserved inventory rebuild access in a compact, collapsible maintenance row.
- Extracted reusable relative-time formatting for Dashboard and Files.
- Kept CLI, archive formats, crypt enforcement, and storage behavior unchanged.

## v0.5.8

### GUI redesign — patch 4

- Rebuilt the Dashboard as a summary-first storage console instead of a provider-card list.
- Added top-level Storage / Files / Health metrics sourced from quota, inventory, and local operational metadata.
- Replaced dashboard provider cards with a compact capacity table using the shared exact-width capacity bar.
- Added pool readiness rows that flag empty pools or references to missing configured crypt remotes.
- Added an Attention section for quota, local metadata, and pool-configuration warnings.
- Added the five most recent task-history records with concise status and relative completion time.
- Added direct navigation links from Dashboard sections to Storage and Jobs.
- Kept storage behavior, CLI behavior, crypt enforcement, and existing work-area screens unchanged.

## v0.5.7

### GUI redesign — patch 3

- Centralized GUI density, spacing, control heights, corner radius, and semantic status colors in `src/gui/theme.rs`.
- Added reusable `capacity_bar`, `status_badge`, `section_header`, and `toolbar` widgets.
- Moved provider usage rendering to the shared capacity bar while preserving exact `used / total` pixel-width behavior.
- Applied the shared status badge to provider availability and task-console state.
- Applied the global GUI style once from the eframe creation context while preserving the active light/dark theme.
- Kept screen ownership, storage behavior, CLI behavior, and navigation structure unchanged.

## v0.5.6

- Replaced the feature-by-feature GUI sidebar with six top-level work areas: Dashboard, Files, Storage, Jobs, Maintenance, and Settings.
- Grouped Inventory/Upload/Restore/Verify/Status under Files without changing their command behavior.
- Grouped Providers/Pools under Storage.
- Added a Jobs area for the current operation and history actions; raw task output remains in the existing bottom console.
- Grouped Integrity/Manifest/System diagnostics under Maintenance; moved inventory maintenance into Files.
- Reorganized GUI screen source files into `screens/files/`, `screens/storage/`, and `screens/maintenance/` folders.
- Preserved existing storage, crypt, manifest, task-runner, and CLI behavior.

## v0.5.5

### GUI refactor — patch 1

- Started the staged GUI redesign without changing visible workflows or storage behavior.
- Split the former monolithic `src/gui/state.rs` into `src/gui/state/app_state.rs`, `persistence.rs`, and `selection.rs`.
- Extracted sidebar navigation ownership into `src/gui/navigation.rs`.
- Added `src/gui/theme.rs` as the single owner for shared GUI layout constants.
- Moved persisted startup-state assembly out of `app.rs`; `app.rs` now focuses on eframe lifecycle and top-level screen orchestration.
- Added `GUI_REDESIGN.md` to keep the multi-patch GUI migration plan in the repository.

## v0.5.4

### GUI as the default entry point

- Running `rpool` without a subcommand now launches the native GUI.
- Global options remain usable with the default GUI path, e.g. `rpool --rclone <path>`.
- `rpool gui` remains supported for scripts and existing wrappers.
- All explicit CLI commands keep their previous behavior.

## v0.5.3

### dotpush/dotpull configuration portability

- Added `rpool config export <file>` and `rpool config import <file>` with `--dry-run` validation.
- Portable bundles contain named pools, `remote_roots.json` data, and portable GUI defaults.
- Machine-local `rclone` executable/path is preserved during import.
- Inventory, task history, resume journals, and rclone credentials are intentionally excluded from the portable bundle.
- Import validates the bundle before writes and attempts rollback to the previous local configuration if an apply step fails.
- Added the `rconfig` Nushell wrapper and `examples/dot-rpool-sync.nu` adapter for pre-`dotpush` capture and post-`dotpull` restore hooks.
- Existing dot-side `rclone.conf` age encryption remains the owner of rclone credentials; rpool does not duplicate those secrets.

## v0.5.2

### Per-remote base paths

- Added persistent `remote_roots.json` configuration for provider-specific default/base paths.
- Added `rpool remote-root list|set|remove` and the `rroots` Nushell helper.
- GUI Settings can add/remove per-remote default paths; crypt target discovery uses the per-remote value before the global GUI fallback folder.
- Capacity discovery now applies provider-specific base paths to physical remotes.
- Fixed crypt/chunker capacity resolution so a configured backing path such as `Instance:/data/crypt` is preserved instead of being collapsed to `Instance:`.
- Bare pool/CLI destination bases inherit configured defaults; explicitly supplied paths remain unchanged.
- Provider health, usage, free-ratio placement, manifest target resolution, and provider drain share the same remote-base resolution rules.
- `rpool doctor` validates remote-root configuration and warns about entries for unknown rclone remotes.
- The mandatory rclone-crypt write boundary from v0.5.1 remains unchanged.

## v0.5.1

### Encrypted storage enforcement

- Remote writes now require an rclone `crypt` destination with data encryption enabled at the storage I/O boundary. This covers data shards, parity, manifest replicas, repair uploads, and provider migration destinations.
- `put` validates all target remotes before planning/upload; pool creation rejects non-crypt destinations.
- Provider drain validates the destination crypt remote before copy, and validates a retained source when it would receive an updated manifest replica.
- Doctor reports pools that reference non-crypt storage targets.
- GUI upload, Pool, and manifest-recovery destination discovery now shows only configured crypt remotes.

### Capacity reporting

- Added rclone backend-type discovery from `rclone config dump`.
- Capacity views exclude `union`, `crypt`, `chunk`, and `chunker` virtual layers.
- Crypt/chunker targets resolve to their physical backing remote for usage, status, provider quota, and free-ratio placement.
- Quota normalization can derive total/free/used from the other two reported fields when a provider omits one value.
- Free-ratio placement shares quota accounting between crypt remotes that resolve to the same physical backing remote.
- Replaced the GUI stock progress bar with an exact pixel-width `used / total` bar.

## v0.5.0

### Continuous integrity

- Added `rpool scrub` with full BLAKE3 verification by default, quick size/existence mode, JSON reporting, recoverability summaries, and optional automatic repair.
- Added `rpool repair` for explicit Reed-Solomon reconstruction of missing, wrong-size, or corrupt physical shards. Repair covers both data and parity shards, validates reconstructed BLAKE3 locally, and verifies the archive again after writes.
- Added `--dry-run` for repair planning.
- Automatic repair refuses to treat transport/provider probe errors as erasures; provider health must be restored before repair proceeds.

### Provider operations

- Added `rpool provider health` for accessibility, latency, quota, pool-scoped checks, and JSON output.
- Added `rpool provider drain` with copy-first migration, full BLAKE3 verification at the new location, atomic local manifest replacement, manifest replica refresh, and optional source deletion.
- Source deletion is opt-in with `--delete-source` and occurs only after copy verification and metadata update.
- When the source provider is retained, its manifest replica is refreshed to the new layout so stale valid manifests are not left behind.
- Provider drain requires manifest v2 and reports that limitation explicitly for legacy v1 metadata.
- Drain refuses layouts that weaken single-provider failure safety unless `--allow-risky` is explicitly supplied.

### Resume and journals

- Added upload completion journals so interrupted uploads retain per-shard completion metadata in addition to the deterministic upload plan.
- Upload journal entries are fully revalidated against remote object size and BLAKE3 before reuse.
- Restore resume state is versioned and stores the expected original size/update timestamp.
- Restore resume now re-hashes every previously completed output range against the manifest before skipping it, automatically invalidating damaged partial output.

### GUI and architecture

- Added GUI **Integrity** screen for scrub/repair workflows.
- Added GUI **Providers** screen for provider health and drain/migration workflows.
- Added dedicated `maintenance/`, `provider/`, and `journal/` domains plus feature-specific CLI/GUI files.
- Manifest format remains v2; v0.5 maintenance features do not require archive migration.
- Cargo check/build remain outside the requested project validation workflow.

## v0.4.1

### Packaging

- Converted the package to an explicit binary-only Cargo target with `autolib = false`, `autobins = false`, and one `[[bin]]` named `rpool`.
- Moved the former crate entry/dispatch from `src/lib.rs` to `src/application.rs`; `main.rs` now registers root modules and starts the application.

### Compatibility fixes

- Updated the GUI for eframe/egui 0.36.2: `eframe::App::ui`, `egui::Panel::{top,left,bottom}`, and `&mut egui::Ui` panel roots.
- Replaced the removed panel `default_width/default_height` usage with `Panel::default_size`.
- Updated the v2 content-root `None` coding branch to an explicit block.
- Removed unused module re-exports and an unused presentation import reported by Rust.

## v0.4.0

v0.4.0 starts the manageability and metadata-safety phase documented in `ROADMAP.md`.

### Pools

- Added persistent named storage pools in the shared rpool config directory.
- `rpool pool list|show|set|remove` manages pool definitions.
- Pools contain remotes plus shard size, workers, retries, placement, and Reed-Solomon K+M defaults.
- `rpool put --pool <name>` resolves the complete upload policy from the pool.
- Explicit put options can override pool defaults; `--pool` and explicit `--remote` are mutually exclusive.
- The GUI Upload page can select an existing pool and then delegates upload policy to that pool.
- Added a GUI Pools screen for creating, loading, editing, and removing pool definitions using the same pool-domain code as the CLI.

### Manifest replicas

- Extracted manifest replication into reusable manifest-domain code.
- Uploads continue to place a full manifest replica on every configured destination.
- Added `rpool manifest verify` to validate and compare provider copies against a reference manifest.
- Added `rpool manifest replicate` to repair/add replicas.
- Added `rpool manifest recover` to restore a local manifest from surviving provider copies.
- Added reusable remote-object read support under `storage/`.
- Added a GUI Manifest replicas screen for verify/replicate/recover workflows.

### Inventory

- Added a local JSON inventory derived entirely from manifests.
- Upload and manifest recovery automatically refresh the cache when possible.
- Added `rpool inventory add|rebuild|list|find|info`.
- Rebuild recursively indexes `.rpool.json` and `manifest.json` candidates and skips invalid candidates.
- The inventory remains a cache; manifests remain the source of truth.

### Task history

- Added append-only JSONL operation history.
- Commands record operation id, type, target, start/end timestamps, status, and a compact error summary.
- CLI argument strings, credentials, and tokens are not persisted.
- Added `rpool history list` and `rpool history prune`.
- GUI child operations are automatically covered because they execute through the same CLI path.

### Doctor

- Added `rpool doctor` and `rpool doctor --json`.
- Added a GUI Maintenance screen for Doctor, inventory list/rebuild, history list, and history pruning.
- Diagnostics cover rclone availability/version, configured remotes, pool validity and remote references, config paths, inventory readability, and history readability.

### Architecture

- Added `ROADMAP.md` as the persistent development plan.
- Added reusable application config path handling under `config/paths.rs`; GUI settings now use it too.
- New domains follow the feature-per-file/folder grouping rule: `pool/`, `inventory/`, `history/`, `doctor/`, nested command folders, and feature-specific CLI schema files.
- Cargo check/build is intentionally not part of this project's requested validation workflow.

## v0.3.0

- Added the native egui/eframe storage console.
- Added Overview, Upload, Restore, Verify, Status, and Settings GUI screens.
- Added background child-process execution, live stdout/stderr, and process-tree cancellation.

## v0.2.0

- Added Reed-Solomon erasure coding and provider quota reporting.
- Added automatic restore reconstruction and failure-domain analysis.

## v0.1.0

- Initial sharded rclone storage implementation.
