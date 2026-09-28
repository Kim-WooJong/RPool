> Latest source delivery: [2026-09-26 handoff](source-delivery-20260926.md). Supersedes packaging and GUI status below.

# M1 storage pivot — final source handoff

> Post-handoff update: [runtime-validation.md](runtime-validation.md) supersedes OS/tool results below. macOS and Linux ARM64 real-tool tests now pass (11 each); Windows and GUI remain unverified. Original checkpoint details below are historical.

2026-09-24. **Scoped M1 source-complete; runtime_verified=false; release_ready=false.**
Version 0.5.15 unchanged; 0.5.16 reserved for Settings/UI Persistence.
This completes the bounded 0–12 migration plan, not native encrypted production storage,
Windows LocalBackend, distributed metadata, or cross-platform release qualification.

## Completion evidence by milestone

- 0–1: pivot-baseline.md, current.md (historical baseline), target.md, ADR-001–006,
  migration.md, compatibility.md. Input ZIP fingerprint retained as historical evidence.
- 2: storage traits/error/reference/capabilities/registry and models/volume; object-safe
  synchronous contract, checked ranges, typed errors. Unused models facade retired at 11.
- 3: storage/memory and faults; deterministic failure injection and real trait tests.
- 4: storage/local; exclusive-owned-root Unix synthetic backend. macOS exercised;
  Windows implementation absent, Linux execution pending (not full 3-OS support).
- 5: storage/rclone process owner and fixture tests; bounded subprocess/error classification,
  crypt checks in write/copy primitives. See step5-rclone.md for cancellation/config limits.
- 6: storage/reader, commands/get_tests; real injected read/verify/plain+RS restoration,
  resume revalidation, routing error vs missing-object distinction.
- 7: storage/writer/source/writer_tests, journal/upload, erasure/encode, maintenance/repair,
  provider/migrate; snapshot consistency, full-hash reuse, encryption gate, verified copy,
  manifest/replica ordering and dry-run protections. Not a distributed transaction.
- 8: storage/admin, provider health/capacity, doctor --local-only, GUI usage/pool background
  execution. Unknown failure independence is not advertised as safe.
- 9: storage/opendal; optional actual SDK Memory adapter behind test+feature gate.
  Shared contract contains no SDK types. Production native route remains disabled.
- 10: manifest/fixtures + compatibility_tests; fixed v1/v2 roots, raw replica/recovery
  byte preservation. 23 crypt/config/manifest core files match input ZIP audit hashes.
- 11: step11-cleanup.json, explicit update-cleanup.nu; obsolete wrappers removed and tests
  moved to actual services. No blanket unused/dead-code suppression remains.
- 12: this handoff, final-inventory.json, reconciled pivot-state and verification records.
  Expert final gate/state audit found no scoped source blocker; stale claims corrected.

## Validation

Final macOS arm64 rerun: default 150 passed, feature 161 passed, zero failed;
10 real-tool tests ignored in each configuration. Both builds succeed with --locked
--offline --bin rpool. Warnings remain 10 test / 46 build. This is not warning-free.
The one test removed at Step11 exercised only a retired test-private retry helper.
Nushell cleanup dry-run passed; no obsolete files remain. No real rclone/age/cloud run.

## Dependency and delivery boundaries

OpenDAL =0.59.3 (MSRV 1.91) and Tokio rt-multi-thread are optional, defaults disabled;
lock resolves Tokio 1.53.1. Unix dev rustix supports the Local synthetic backend.
Default dependency tree excludes OpenDAL/Tokio. Cargo.lock is committed source material;
no version bump or unrelated dependency upgrade was intended. Details in step9-opendal.md.

final-inventory.json compares scoped source/docs/scripts/config against the original ZIP,
with SHA-256 for changed/added files and baseline hashes for removed files. It does not
represent target binaries, credentials, user archives or unrelated workspace files.
Step11 removed 15 files; baseline-relative deletion is 13: two retired files were introduced
and removed within this pivot. Cleanup script retains all explicit retired paths for
intermediate-tree upgrades. No delivery ZIP was requested or created.

## Runtime/release gates and next actions

1. **Windows first**, then Linux: execute existing default/feature build+test CI matrix.
   Resolve failures before claiming portability; macOS results cannot substitute.
2. Run ten ignored B6 rclone/age cases in isolated temporary test configuration, never
   real production config; capture exact versions and results per OS. No cloud needed
   for local crypt fixtures. Required tools/environments are not supplied by this handoff.
3. Exercise GUI progress, cancellation, pool save/reload and session-local retry on each OS.
4. Review retained warnings and production integration debt without enabling unfinished
   crypto/native features simply to silence warnings.
5. Reassess runtime_verified/release_ready only after evidence is recorded. MetadataStore
   authority/fencing/CAS and native encryption are separate future design blockers, not
   features to activate during release verification.

Rollback concerns source/config compatibility; no automatic remote-data rollback promised.
M1 source handoff is done. Remaining runtime work is not silently marked complete.
