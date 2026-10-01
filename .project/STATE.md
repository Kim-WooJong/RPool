# Project State

Updated: 2026-10-01

## Project

RPool is a Rust GUI/CLI sharded archive tool and online drive over encrypted
rclone remotes. Canonical source is the Git checkout `artifacts/rpool`; build and
test only in working copies under `projects/rpool/` with `CARGO_TARGET_DIR`
outside the source tree. Windows is the primary intended GUI platform; executed
tests run on macOS and Linux (Docker). Version 1.0.0 (2026-10-01); later work goes under
"Unreleased" in CHANGELOG.md, which is the detailed history.

## Current Status

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
  compiled, never run on Windows. macOS has no native frontend; the old macOS
  4 GiB NFS test workspace (`projects/rpool/4g-e2e-fdb991f6/`) is blocked on OS
  recovery and must not be reused or cleaned.
- Pool sync is eventual; no distributed locking or cross-PC open-handle
  coherence; empty folders are local; payload history accumulates until a
  reclamation exists.
- `put` to crypt over S3-like bucket backends fails (missing key stats as a
  directory).
- WebDAV write amplification for repeated full-prefix PUTs is mitigated by the
  60 s VFS write-back, not bounded (`docs/MOUNT_WRITE_ROADMAP.md`).
- Pool migration phase 3 (drive adoption) is unit-tested, not yet run with real
  remotes on several PCs.

## Latest validation

2026-10-01 documentation cleanup (this working copy): `cargo build` passed;
`cargo test --locked --bin rpool`: 890 passed / 0 failed / 44 ignored with a non-symlink TMPDIR (with `TMPDIR=/tmp`, a symlink on macOS, 3 lock/path tests fail, a known fixture limitation). Docker and cloud runs were
not repeated for this change.

## Resume

Next work from ROADMAP.md: Windows runtime validation of WinFsp, drive data
reclamation, macOS native frontend after the NFS host is recovered, bucket
backend `put`. Keep the old 4 GiB Mac test state untouched.
