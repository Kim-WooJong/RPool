# Post-handoff runtime validation — 2026-09-24

## Actual results

- macOS 26.4 ARM64: default 150 passed; OpenDAL feature 161 passed; 11 ignored
  real-tool tests separately executed: **11 passed, 0 failed**. Both builds pass.
- Linux ARM64 Debian container on Docker Desktop: default 150 passed; feature 161 passed;
  11 real-tool tests separately executed: **11 passed, 0 failed**. Both builds pass.
- Windows: **not executed**, no connected Windows runtime/repository CI runner supplied.
- Interactive GUI: **not executed**. Linux container checks are not desktop GUI validation
  or x86_64 coverage. Warnings remain 10 test / 46 build.

Pinned successful tools: rclone 1.75.1, age + age-keygen 1.3.2. Official GitHub release
assets for darwin-arm64/linux-arm64 verified against release asset SHA-256 digests.
Tools downloaded to task-specific /tmp directories, not installed into host system paths.
Linux tools installed only into isolated task container. No production config or cloud used.

## Failures found and resolved

1. First macOS B6 run passed 10/10; rerun failed one fixture setup. Synthetic reproduction
   showed a leading '-' obscured password interpreted as a CLI option (exit 2 vs fixed exit 0).
   Both test config creation and production encrypted-stage config update now place all
   options before '--'. Secret bytes are neither escaped nor regenerated. New B6 test
   obtains a valid leading-hyphen obscured synthetic value and verifies create/update exact
   readback using the production argument builder. This does not establish complete encrypted
   config transaction coverage; the regression uses a plaintext temporary config.
2. Linux distro rclone 1.60.1-DEV / age 1.2.1 failed all ten fixture initializations.
   rclone config create help has no --no-output support used by fixtures. Pinned current
   tools pass. No general minimum version or age 1.2.1 defect is established.
3. One macOS flood fixture hit the shared 3-second deadline while Linux compilation ran.
   Isolated rerun passed; contention is plausible, not proven. Expert review confirmed this
   tests drain correctness, not performance. Only flood test uses fresh 30-second budgets;
   oversize error must be InvalidInput, not any error. Actual stalled-I/O tests retain 3s.
4. Runner initially resolved cargo symlink into rustup, changing argv[0] dispatch. It now
   retains the cargo filename and explicitly uses the requested toolchain (default nightly).

## Reproduce on Windows

Install Rust nightly and trusted rclone/age binaries, then from repository:

```powershell
cargo +nightly test --locked --bin rpool
cargo +nightly test --locked --features opendal-prototype --bin rpool
cargo +nightly build --locked --bin rpool
cargo +nightly build --locked --features opendal-prototype --bin rpool
python scripts/validate-runtime.py --rclone C:\tools\rclone.exe --age C:\tools\age.exe --age-keygen C:\tools\age-keygen.exe
```

Runner removes inherited RCLONE_/_RCLONE_/RPOOL_TEST_ overrides, supplies explicit tools,
performs version preflight and applies an outer timeout. B6 uses disposable local configs,
crypt backing directories and identities. The new prefix fixture searches at most 2048
random-IV obscures; no secret value/command stderr is printed.

## Remaining gates and artifact scope

Windows execution and per-OS GUI interaction still block release qualification.
Linux success covers aarch64 only. No hosted GitHub workflow was published/triggered.
Existing workflow still skips B6 by default; explicit runner invocation is required.
LocalBackend remains Unix-test-only; OpenDAL remains test-only Memory; distributed metadata
and native production encryption remain deferred. runtime_verified/release_ready stay false.

Step10 baseline audit and Step12 final-inventory are historical snapshots, not current
post-validation hashes. crypt_restore.rs and B6 test file now intentionally differ due to
this demonstrated correctness fix. Current changed source hashes are in runtime-validation.json.
No user secret, binary, target directory or user archive included in handoff source records.
