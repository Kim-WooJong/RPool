# Project State

Updated: 2026-10-16

## Project

RPool is a Rust GUI/CLI sharded archive tool and online drive over encrypted
rclone remotes. Canonical source is the Git checkout `artifacts/rpool`; build and
test only in working copies under `projects/rpool/` with `CARGO_TARGET_DIR`
outside the source tree. Windows is the primary intended GUI platform; executed
tests run on macOS and Linux (Docker). This Mac Studio currently has no RPool pool
in use: it is for development and local/fake-storage testing, not live-pool
validation. Version 2.19.1 (2026-10-16); later work goes under
"Unreleased" in CHANGELOG.md, which is the detailed history.

## Current Status

macOS mount hang fix (2026-10-15, source + local tests only). Reported: a
mount works, then unmounts by itself and Finder/terminal/the Mac freeze. The
last Mac session (2026-10-07, CLI 2.14.1) died without a lease update; the Mac
was hard-rebooted twice that day; kernel logs had already expired. Root causes
found in source (not reproduced live): print macros panicking on a closed GUI
pipe and unwinding past the stop, which dropped WebDAV under a live rclone NFS
server; hard NFS volumes left behind when rclone ended; leases refusing every
restart after any unclean end; NLM locks to an rclone server without lock
service; namespace waits on the DAV server's two async workers; GUI stat of
mountpoints every frame. All are fixed (see CHANGELOG Unreleased). Still
inherent: rclone 1.75's NFS server (go-nfs v0.0.4) answers one request at a
time, so an uncached cloud read stalls the whole volume until it finishes.
The macFUSE frontend avoids that and now works: it had never mounted because
macFUSE 5's `fuse_mount_compat25` fails, so `auto` (GUI default) always fell
back to WebDAV/NFS. It now mounts through `fuse_mount` (kernel backend first,
FSKit only when enabled), replies to reads off the single session thread, and
was verified on this Mac (macFUSE 5.4 kernel backend) with the kernel-level
FUSE tests and an isolated local-remote e2e (`projects/rpool/fuse-e2e-mac.sh`).
Not yet tested with a live cloud pool or the FSKit backend. macFUSE's
default 60 s `daemon_timeout` ejected the drive on one slow request
(reproduced); mounts now use 600 s and RPool answers slow reads/writes within
480 s from worker threads. Remaining: the macOS kernel locks a file while it is
read, so a cloud-only file being downloaded (whole shard first, see
`shard_cache.rs`) blocks stats/listings that touch that file until the shard
arrives; range reads would need a shard-integrity design decision.

Cloud-synced empty folders are implemented in the current source: mkdir,
last-file deletion or move-out, rename, and rmdir update immutable folder
records under a sibling metadata tree. Updated PCs pull those records; mounted
and unmounted GUI Library views include explicit empty folders. File events
remain compatible with older clients, which keep syncing files but cannot
display or publish the shared empty folders. Folder/file name collisions are
resolved in favor of files instead of blocking pull. Account backfill and
drive-generation adoption carry the folder records. Source-level tests passed
on macOS, but a live cloud cross-PC and Windows WinFsp test are still needed.
Unreadable or newer folder records are skipped during ordinary pull and Library
browse so they cannot block file events; generation migration remains strict.

Long-running RPool GUI/transfer freeze mitigation is implemented but not proven on the
affected Windows PC. Conversion re-upload now reads its private restored file
directly, avoiding an additional full-file temporary snapshot. The GUI now
uses scheduled refresh for the always-visible task indicator and migration
status, budgets task event handling per frame, and bounds all task logs. Expert
source review found that the expanded console also laid out all 2,000 retained
lines per frame; it now lays out visible rows only. eframe's `App::logic` now
polls child output and completion while the window is minimized, with a timer
rescheduled from logic and after visible UI actions start tasks. Migration cloud
status refresh remains a screen/UI operation and may run on restoration.
On the affected Windows PC,
CPU/memory usage was low and working-drive free space was sufficient; the
whole-desktop slowdown was observed when the long-backgrounded GUI was restored.
OpenGL/Windows display-path delay is plausible but unproven. Expert source
review also found potentially high aggregate transfer concurrency and
repeated full journal scans in the *migration* path. Reprocessing instead runs
archives sequentially using the frozen target worker count. Neither path is
established as the cause of the reported whole-PC freeze.

The placement migration change adds explicit `--reapply-placement` / GUI
"Reapply placement (re-upload all files)" to restore and re-upload existing
archives and included drive files under the saved pool placement policy. It
preserves originals until verification and switch. This is distinct from
conservative `--rebalance`, which may correctly plan zero moves after a policy
change. The affected pool is on another PC; no live migration has run here.

The migration run and drive adoption screens now show per-storage upload and
download rates (10-second averages), cumulative bytes, active transfer state,
failures/retries, and provider-side copy bytes for the current CLI run. The
separate migration process sends traffic snapshots through the GUI progress
protocol. `rclone copyto` streamed moves, which bypassed the existing byte
counters, are credited at completion to both source and destination. Last-run
totals stay visible until another task replaces them. These are per-PC,
per-run traffic measurements, not the migration's overall completion fraction.

Placement-policy changes now have explicit effective-time rules: direct `put`
uses the saved policy; a running drive uses its mounted policy until a safe
remount, and saved pending plans keep their original targets until completed.
Existing archives require `--reapply-placement`; the GUI blocks starting a
migration plan whose frozen target placement no longer matches the saved pool.
Free-ratio incremental uploads reject retained shard groups that violate the
current backing-account shard cap. Monitoring names a storage whose upload
failed with explicit provider-full evidence or whose quota preflight found zero
upload space, and preserves the sanitized upload error separately from ordinary
read/listing errors. The GUI also shows these per-storage task results without
a mounted pool. Capacity observations do not prove that other storages are
healthy, nor that a live transfer succeeded.

What exists:

- **Archives:** `put`/`get`/`verify`/`status`/`usage` with optional
  Reed-Solomon (GF(256)), placements round-robin, free-ratio, resilient
  (≤ M shards per declared outage group) and capacity-first; resumable upload
  and restore journals; hedged RS reads; per-remote limits; a persistent
  `rclone rcd` for reads; optional native crypt per pool (rclone crypt format).
- **Online drive:** one mode, the virtual drive with v6 pool sync
  (`rpool mount --pool P --workspace W --mountpoint M [--pool-worker NAME]`).
  Frontends `auto` (default): native FUSE on Linux, WinFsp on Windows builds,
  else rclone mount over RPool's loopback WebDAV (macOS uses `nfsmount`).
  Conflicts keep original + both edits; incremental uploads; WebDAV dirty-cache
  recovery; `--import-from`; `--apply-pool-changes`; account recovery into a new
  pool; several pools mounted at once (CLI and GUI).
  Removed (code, options, GUI): v7 peer snapshots, v5 bounded shared, v3 shared
  root, "this PC only" retention, full local replica, OpenDAL prototype,
  test-only LocalBackend, hidden `--root`, legacy `config export/import`.
- **Drive history:** `rpool drive trash|versions|rollback|retention`; purge and
  expiry hide entries. Data reclamation for v6 (`rpool drive cleanup`) is in
  progress in a parallel work item; see `docs/DRIVE_HISTORY_DESIGN.md`.
- **Drive metadata:** checkpoints written automatically and by
  `rpool pool compact`; covered-record deletion behind `--enable-deletion`.
- **Pools:** `pool set|list|show|remove|capacity|browse|speed-test|compact`,
  pool change migration phases 1–4 (`pool migrate plan|run|status|lost|abandon|adopt|retire|restore`),
  copy-only reprocess.
- **Providers:** health, account limits (daily upload budget, bandwidth
  timetable, tpslimit, inactivity warnings), keepalive, speed test, crypt
  provisioning, drain.
- **Maintenance:** scrub/repair, manifest replicas, inventory, history,
  `doctor` (rclone version check, metadata growth) and `doctor --bundle`
  (redacted diagnostics ZIP).
- **Monitoring:** per-mount traffic status/history, `rpool mount monitor`, GUI
  Monitoring page.
- **Portability:** `rpool export/import` (age-encrypted crypt secrets),
  `rpool config paths`.

How it is verified:

- `scripts/ci-local.sh`: fmt, check, clippy (report-only), the unit/integration
  suite, ignored real-tool tests when rclone/age exist, Windows cross check when
  the target is installed. Suite on macOS (2026-10-01, this working copy):
  see "Latest validation".
- Docker Linux FUSE end-to-end scripts (`scripts/linux-docker/`): FUSE mount,
  two-PC pool sync, cache recovery, layout change, rclone import, browse after
  apply, idle traffic, monitoring, migration plan/run/journal/relocate/
  server-copy. They passed before the mode removal with the pool-sync
  invocation; after the removal the scripts were switched to the plain
  `rpool mount` form and checked with `sh -n` only.
- Real clouds: speed test (7 accounts), pool migration and `put` on Dropbox,
  Koofr, Drime, Filen, SMB/SFTP (see CHANGELOG). Crypt portability real-tool
  tests on macOS ARM64 and Linux ARM64 (2026-09-24).

Known limits:

- WinFsp frontend, Windows seal/flush fix and the Windows rclone daemon path are
  compiled, never run on Windows. macOS native frontend = macFUSE (kernel backend verified 2026-10-15); the old macOS
  4 GiB NFS test workspace (`projects/rpool/4g-e2e-fdb991f6/`) is blocked on OS
  recovery and must not be reused or cleaned.
- Pool sync is eventual; no distributed locking or cross-PC open-handle
  coherence. Older clients do not sync empty folders. Folder record history
  is immutable and not yet compacted; payload history accumulates until a
  reclamation exists.
- `put` to crypt over S3-like bucket backends fails (missing key stats as a
  directory).
- WebDAV write amplification for repeated full-prefix PUTs is mitigated by the
  60 s VFS write-back, not bounded (`docs/MOUNT_WRITE_ROADMAP.md`).
- Pool migration phase 3 (drive adoption) is unit-tested, not yet run with real
  remotes on several PCs.
- There is no RPool pool in use on this Mac Studio. Do not treat local compile,
  unit/integration, or fake-storage results as validation against the user's
  live pool; validate the migration on the PC hosting that pool when available.

## Latest validation

2026-10-15 macOS mount-hang fix: source-matched `projects/rpool/mount-hang-fix`
passed macOS `cargo check --all-targets`, the Windows cross check (1.98.1
toolchain), the three ignored real-rclone loopback WebDAV tests, the full
suite (1,125 passed, 60 ignored, one long randomized trace filtered) and a
warnings-denied release build. No clippy warnings in changed files; changed
files are rustfmt-clean. The new DAV stall test was confirmed to fail on the
previous server. An adversarial expert review's findings were fixed. No live
NFS mount, force-unmount of a real dead server, Linux `ps -A -ww`, or Windows
runtime test was run. The release build is installed in `~/.cargo/bin`
(reports 2.17.0; previous 2.14.1 kept as `rpool.2.14.1.bak` /
`rpool-gui.2.14.1.bak`). Changes are uncommitted, together with the
2026-10-13 work.

2026-10-13 cloud-folder work: source-matched `projects/rpool/` passed macOS
`cargo check --all-targets` and warnings-denied release checking. Folder
projection, namespace journal, two-PC in-memory record exchange, cloud
Library folder-only listing, migration adoption, and mount behavior tests
passed. The final source-matched full suite passed: 1,120 passed, 60 ignored,
one long randomized trace filtered. Windows WinFsp
and live-cloud two-PC behavior are not yet runtime-validated.

2026-10-13 freeze mitigation: source-matched `projects/rpool/gui-freeze-verify`
passed macOS `cargo check --all-targets`, six GUI runner bounded-output tests,
eleven pool-reprocess tests, and one GUI layout test. Changed Rust files passed
`rustfmt --check`; repository-wide `cargo fmt --check` still reports pre-existing differences in
unrelated files. No live-pool, Windows-runtime, or whole-PC freeze reproduction
was available. The subsequent console visible-row change passed macOS
`cargo check --bin rpool`. After moving background polling to `App::logic`,
the source-matched verification copy passed macOS `cargo check --all-targets`
and six GUI bounded-output tests; changed Rust files passed `rustfmt --check`
and `git diff --check`. It has not been tested on Windows.

2026-10-11 placement-policy/monitoring work: source-matched verification copy
passed `cargo check --all-targets` and `cargo test --bin rpool -- --skip
randomized_traces_match_the_reference_model_through_crashes` (1,105 passed,
0 failed, 60 ignored, 1 filtered). This includes changed-placement safe-remount,
pending-plan deferral, free-ratio retained-shard cap, provider-full
classification, and named Monitoring alerts. All locale JSON parsed and
`git diff --check` was clean. No live pool or cloud upload was available on
this Mac, so the actual other-PC placement and full-provider display remain
runtime validation items.

2026-10-11 migration traffic UI: `cargo check --all-targets` passed; traffic,
progress protocol, GUI translation, migration screen, relocation, and layout
tests passed (including 12 storage rows at 440 px). After adding the final
copy-timing hint, all three locale JSON files and its translations were checked
directly; a repeat i18n test run stalled before any test started while macOS
`syspolicyd`/`trustd` were heavily loaded (an independent `codesign` lookup
stalled too). The live pool on the other PC was not used; actual cloud rates
and provider-side-copy behavior still need validation there. Repository-wide
`cargo fmt --check` reports only pre-existing format differences in unrelated
application/config-sync/rclone files; the new migration traffic files were
formatted.

2026-10-11 reapply-placement work: local Rust test suite passed (1,094 tests)
and release compilation passed in a source-matched verification copy. No live
pool migration, cloud transfer, or other-PC runtime validation was performed.

2026-10-01 documentation cleanup (this working copy): `cargo build` passed;
`cargo test --locked --bin rpool`: 890 passed / 0 failed / 44 ignored with a non-symlink TMPDIR (with `TMPDIR=/tmp`, a symlink on macOS, 3 lock/path tests fail, a known fixture limitation). Docker and cloud runs were
not repeated for this change.

## Resume

For the cloud-folder feature, install the updated build on two Windows PCs
with the same pool and verify Explorer mkdir, delete-last-file, move-out,
rmdir, remount, and the unmounted Library. Confirm old clients still sync
files but cannot display the empty folders. Folder-record compaction remains
future work only if its growth becomes material.

On the affected Windows PC, test a build containing the pending GUI changes:
run a migration, minimize the GUI for hours, then restore it. Confirm the child
continues transferring and the GUI completion state is timely, with no large
first-frame output catch-up. Compare the operation console collapsed vs expanded.
If the whole desktop still slows, correlate the incident with Windows
display-driver/DWM events and GUI-thread OpenGL present time before changing
renderer or migration concurrency. Do not treat the local code changes as a
confirmed fix for the whole-PC freeze.

For placement migration, start from `src/migration/plan.rs` and the current
implementation in the canonical checkout. Keep the live-pool validation limitation above
explicit; the active pool is on another PC, not this Mac.

Next work from ROADMAP.md: Windows runtime validation of WinFsp, drive data
reclamation, macOS native frontend after the NFS host is recovered, bucket
backend `put`. Keep the old 4 GiB Mac test state untouched.
