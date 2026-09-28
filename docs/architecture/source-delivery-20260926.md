# M1 final source delivery — 2026-09-26

This is a source-only handoff, not a release-qualified binary. Version remains 0.5.15; 0.5.16 remains reserved for Settings/UI Persistence. Initial-setup is maintained separately and was not modified.

## Current validation

macOS ARM64, locked/offline nightly toolchain: default tests 150 passed, optional OpenDAL tests 161 passed, zero failed; both builds succeeded. Each normal suite excludes 11 real-tool tests. Warnings remain 10 in tests and 46 in builds. No blanket suppression or feature deletion was used to silence warnings.

The 2026-09-24 runtime report separately records macOS/Linux ARM64 builds and tests plus 11 real rclone/age tests passing on each OS. Those tool tests were not rerun for this documentation/packaging-only delivery. No Rust source changed in this finalization.

GUI smoke: launched current binary using temporary HOME/XDG_CONFIG_HOME/APPDATA, an explicitly nonexistent rclone executable, and no production settings. Process remained alive for eight seconds without log errors, then was terminated by the harness. This is process-startup evidence only: rendering/navigation/cancellation/pool save/reload were NOT interactively verified.

## Warning and GUI review

Independent review found no must-fix defect established solely by retained warnings. Keep crypt recovery opt-in; do not activate dormant recovery or native storage to silence warnings. Typed IDs/capabilities and future operations remain contract scaffolding.

Visible structured-progress rendering and session-local retry are not wired into the current GUI (progress_view has no caller, retry_last is unused). These are outstanding GUI integration items, not validated delivered features. GUI interaction and Windows runtime remain pending. Therefore runtime_verified=false and release_ready=false remain unchanged.

## Delivery

ZIP contains source, Cargo.toml/Cargo.lock, documentation, examples, scripts and CI configuration. It excludes target/, the old root rpool.exe, runtime state and other projects. Build Windows binaries from this source; do not use the excluded stale executable as evidence for current source.

The adjacent manifest lists each archived file's SHA-256; SHA256SUMS covers the ZIP and manifest. Archive integrity and extracted-file hash equality were verified after creation. Historical final-inventory.json remains a baseline comparison, not this delivery's complete manifest.

## User Windows checks

Run locked default and opendal-prototype test/build configurations, then scripts/validate-runtime.py with explicit rclone/age/age-keygen executable paths. See runtime-validation.md for isolated tool prerequisites. Finally exercise GUI navigation, task completion/failure/cancel, and pool save/reload with test data. Do not infer interactive success from the startup smoke.
