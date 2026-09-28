# Storage pivot verification — Step 11

> Post-handoff update: [runtime-validation.md](runtime-validation.md) supersedes OS/tool results below. macOS and Linux ARM64 real-tool tests now pass (11 each); Windows and GUI remain unverified. Original checkpoint details below are historical.

2026-09-24, macOS arm64, Rust/Cargo 1.96 nightly. Version remains 0.5.15.
Source milestones 0–12 complete within the scoped synthetic/legacy pivot; final handoff recorded in final-handoff.md. Not release-ready.

## Executed

All Cargo commands: --locked --offline --bin rpool.

- Default cargo test: **150 passed, 0 failed, 10 ignored**.
- opendal-prototype cargo test: **161 passed, 0 failed, 10 ignored**.
- Default and feature-enabled cargo build: success.
- Warning diagnostics: **10 test / 46 build**, previously 24 / 60.
- nu scripts/update-cleanup.nu --root . --dry-run: success, 0 obsolete files.
- Step10 crypt/config + manifest core hash audit: all 23 files unchanged.
- No blanket allow(dead_code)/allow(unused...) remains under src.

## Cleanup evidence

See step11-cleanup.json for exact modified/deleted paths and retained warnings.
15 files removed: 9 retired storage bridges, unused models storage facade, 5 historical
GUI/model remnants. Only explicit paths added to update-cleanup.nu; existing root/name
checks and dry-run preserved. No target, executable, credentials or user archive removed.
Unused maintenance wrappers, remote-config helpers, ec_temp_root, raw metadata helper and
prelude exports removed. Provider test-only reexport uses cfg(test).
Two rclone fixture tests now exercise actual StorageReader/Writer including copy_verified,
NotFound vs auth errors, metadata, BLAKE3, and offset/truncating downloads.
One test removed with transfer.rs: its private retry helper tested only itself, not the
production retry paths. Actual unknown-outcome/retry tests in writer/rclone remain.
Expert caller audit and concrete cleanup review cover active services/platform boundaries.

## Retained warnings and limits

Do not equate warning elimination with correctness. Crypt generation/interrupted recovery
and GUI retry/progress APIs are preserved rather than deleted to silence diagnostics.
Future domain IDs, capabilities, registry injection and optional contract methods remain
visible warnings after their former broad suppression was removed. No feature was wired
merely to silence a warning. See exact warning inventory for the remaining integration debt.

Windows is primary design target, but Windows/Linux CI has **not run**. LocalBackend is
Unix-test-only (macOS exercised, no Windows sandbox). OpenDAL is default-off Memory test
prototype; no production native encryption route or Fs sandbox. GUI interaction untested.
Ten B6 real rclone/age tests remain ignored; no cloud or real-tool integration executed.
Source snapshots require temporary disk capacity. Provider failure-domain independence,
crypt config external edit races, cooperative I/O cancellation and distributed metadata
coordination remain limitations; see limitations.md and ADR-003. No crash durability or
multi-writer/consensus guarantee. No remote data automatic rollback promise.
