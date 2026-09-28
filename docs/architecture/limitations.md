> Latest source delivery: [2026-09-26 handoff](source-delivery-20260926.md). Supersedes packaging and GUI status below.

# Current storage pivot limitations

> Post-handoff update: [runtime-validation.md](runtime-validation.md) supersedes OS/tool results below. macOS and Linux ARM64 real-tool tests now pass (11 each); Windows and GUI remain unverified. Original checkpoint details below are historical.

- release_ready=false; final source handoff complete, runtime gate pending.
- macOS arm64 builds/unit+fixture tests only; Windows/Linux CI execution pending.
- 10 ignored real rclone/age tests and interactive GUI checks outstanding.
- Default storage still requires rclone crypt. OpenDAL Memory is test-only/default-off;
  no native encrypted production writes, no OpenDAL Fs path isolation guarantee.
- LocalBackend is Unix synthetic-only; Windows reparse-safe implementation absent.
- Source snapshot and shard spools require extra disk; full hash verification adds I/O.
- Remote writes/manifest replicas are ordered, not a distributed atomic transaction.
- Distinct remote names do not establish independent failure domains. Unknown RS migration
  safety requires allow-risky. Quota remains conservative without account independence.
- Config may be edited externally after crypt validation. Process owner reaps its direct
  child; arbitrary blocking callbacks are cooperative, not forcibly interruptible.
- MetadataStore authority/CAS/fencing, failover, quorum, reader/GC races and multi-writer
  guarantees remain ADR-003 blockers, not delivered functionality.
- Retained warnings are enumerated in step11-cleanup.json; no warning-free claim.
- Original manifest bytes are retained for explicit replication/recovery; changed manifests
  intentionally serialize. Rollback is source/config compatibility, not remote data rollback.
