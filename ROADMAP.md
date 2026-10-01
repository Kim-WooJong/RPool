# rpool development roadmap

The working plan for rpool. Update it when scope changes. Release history is in
[CHANGELOG.md](CHANGELOG.md); the current state is in `.project/STATE.md`.

## Development principles

1. One feature owns one source file wherever practical.
2. Reusable logic lives in shared modules rather than being copied between CLI and GUI code.
3. Related features are grouped into domain folders.
4. Storage metadata remains recoverable and portable; caches must be rebuildable from manifests.
5. Backward compatibility is preferred for manifests and command behavior unless an incompatible change is explicitly documented.
6. Validate in-scope changes with Cargo checks/tests/builds; distinguish local results from unverified platforms and real-cloud operation.
7. Batch version changes according to [version policy](docs/VERSIONING.md).
8. Every CLI change ships with the matching GUI change.

## Done

- **v0.4 manageability:** named pools, manifest replica verify/replicate/recover,
  rebuildable inventory, task history, doctor.
- **v0.5 continuous integrity:** scrub and repair (data and parity), provider
  health, provider drain, verified resume journals, encrypted-only writes,
  per-remote default paths, GUI-default startup, GUI redesign (work areas,
  theme, dashboard, files, upload, jobs, storage, maintenance UI), crypt secret
  portability (`rpool export/import`).
- **v0.6:** provider connection and automatic crypt setup, encryption defaults,
  pool selection, copy-only reprocessing.
- **v0.8:** native crypt writes per pool; filesystem core with crash and trace
  tests; native Linux FUSE frontend (Docker-verified) and Windows WinFsp
  frontend (built); `--frontend auto` default.
- **Unreleased (see CHANGELOG):**
  - one drive mode (virtual drive with v6 pool sync); all other modes removed;
  - enforceable failure-domain policy (resilient placement) and
    capacity-first placement;
  - snapshots/versioning for the drive: trash, file versions, rollback;
  - incremental uploads (dedup of unchanged shards/groups of the parent
    revision);
  - pool change migration phases 1–4 (archives, server-side copies, drive with
    epoch adoption, cleanup with quarantine) — the rebalance/move path;
  - metadata checkpoints and compaction;
  - bandwidth/API-rate scheduling: per-account daily upload budgets,
    bandwidth timetable, tpslimit, keepalive and inactivity warnings;
  - network monitoring of mounted pools, storage speed test, diagnostics
    bundle, rclone version check.

## Open

- **Windows runtime:** WinFsp frontend (listing, reads, stop, small writes,
  remount, recovery, 4 GiB 9+3 round trip), Windows seal/flush fix, Windows
  rclone daemon path.
- **Drive data reclamation:** free payloads of purged/expired trash entries and
  old versions safely across PCs (`rpool drive cleanup`, see
  [DRIVE_HISTORY_DESIGN.md](docs/DRIVE_HISTORY_DESIGN.md)).
- **macOS native frontend** (M6 in
  [NATIVE_MOUNT_CRYPT_PLAN.md](docs/NATIVE_MOUNT_CRYPT_PLAN.md)), after the
  stuck NFS test host is recovered; WebDAV write-trace gates in
  [MOUNT_WRITE_ROADMAP.md](docs/MOUNT_WRITE_ROADMAP.md).
- **Real multi-PC validation** of drive migration adoption and long-running
  pool sync on real providers.
- **Bucket backends:** `put` to crypt over S3-like buckets (missing key stats as
  a directory).
- **Drive features:** one-click conflict resolution, modification times in the
  drive, cross-version deduplication beyond the parent revision.
- **Automation:** cost-aware placement, storage growth analytics, scheduled
  maintenance (scrub, compaction policy).

Metadata migrations must be added before format changes that require them.
Choose the next bounded task from user priorities; assign its version only
when a validated batch is ready.

## Long-term invariants

- A catalog/database may accelerate lookup but must be rebuildable from manifests.
- A single cloud/provider must never be treated as the only metadata authority.
- Corruption detection is checksum-based; missing/corrupt shards are erasures for recovery purposes.
- Repair and migration should preserve the configured failure-domain policy throughout the operation where possible.
- Destructive maintenance operations provide a dry-run or preview before deletion.
