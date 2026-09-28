# rpool development roadmap

This file is the working development plan for rpool. It should be updated when scope changes so that the implementation and maintenance plan remain visible inside the repository.

## Development principles

1. One feature owns one source file wherever practical.
2. Reusable logic lives in shared modules rather than being copied between CLI and GUI code.
3. Related features are grouped into domain folders.
4. Storage metadata remains recoverable and portable; caches must be rebuildable from manifests.
5. Backward compatibility is preferred for manifests and command behavior unless an incompatible change is explicitly documented.
6. Validate in-scope changes with Cargo checks/tests/builds; distinguish local results from unverified platforms and real-cloud operation.
7. Batch version changes according to [version policy](docs/VERSIONING.md); old reservations in historical documents no longer apply.

## v0.4 — manageability and metadata safety

**Status: implemented in v0.4.0.** GUI upload supports named pools; the management/maintenance commands are available through the CLI and are designed for later GUI surfacing without duplicating core logic.

### 1. Pool abstraction

Goal: stop repeating provider lists and storage policy on every upload.

Planned interface:

```text
rpool pool list
rpool pool show archive
rpool pool set archive --remote nas-crypt:rpool --remote gdrive-crypt:rpool --remote sftp-crypt:rpool
rpool pool remove archive
rpool put file.iso --pool archive
```

A pool owns reusable defaults for remotes, shard size, workers, retries, placement, and Reed-Solomon K+M.

### 2. Manifest replication management

Goal: make manifest redundancy observable and repairable instead of relying only on copies written during upload.

Planned interface:

```text
rpool manifest verify file.rpool.json
rpool manifest replicate file.rpool.json
rpool manifest recover <archive-id> --pool archive
```

Manifest copies remain simple full replicas because manifests are small and are more useful when independently readable.

### 3. Inventory/catalog

Goal: provide a searchable local index without making the index the source of truth.

Planned interface:

```text
rpool inventory add file.rpool.json
rpool inventory rebuild <manifest-directory>
rpool inventory list
rpool inventory find '*.iso'
rpool inventory info <archive-id>
```

The catalog is a rebuildable cache generated from manifests.

### 4. Task history

Goal: preserve operational history for CLI and GUI tasks.

Each record should include operation id, operation type, start/end time, target, status, and a short result/error summary. Credentials and tokens must never be written to history.

### 5. Doctor / diagnostics

Goal: detect configuration and operational problems before data operations fail.

Checks should cover:

- rclone executable and version invocation
- configured rclone remotes
- pool configuration validity
- metadata/config directories
- manifest/inventory/history readability
- obvious provider configuration mismatches

## v0.5 — continuous integrity

**Status: implemented in v0.5.0; encryption/capacity hardening shipped in v0.5.1, path-sensitive remote-base resolution in v0.5.2, and portable dotpush/dotpull configuration synchronization in v0.5.3, and GUI-default startup in v0.5.4.**

### 1. Scrub

`rpool scrub <manifest>` performs a full BLAKE3 scan by default. `--quick` limits the scan to existence/size checks. Reports classify healthy, missing, wrong-size, corrupt, and transport-error shards and summarize degraded/unrecoverable coding groups.

### 2. Automatic repair

`rpool repair <manifest>` and `rpool scrub <manifest> --repair` reconstruct recoverable data **and parity** shards with the existing Reed-Solomon metadata, verify reconstructed BLAKE3 hashes locally, upload them to their original object paths, and run post-repair verification. `--dry-run` is available before writes. Unknown transport/provider errors are not automatically classified as erasures; repair stops until provider health is resolved.

### 3. Provider health monitor

`rpool provider health` checks rclone accessibility/latency and quota information for all configured remotes or a named pool. JSON output is available for automation. The GUI exposes the same operation.

### 4. Provider drain/migration

`rpool provider drain` copies shards from one provider base to another, fully verifies the copied objects, rewrites and replicates the updated manifest, then optionally deletes the source objects. Deletion is never implicit. Migrations that weaken single-provider failure safety are rejected unless explicitly permitted.

### 5. Stronger resume/journal handling

Uploads keep a per-source completed-shard journal alongside the existing upload plan. Journal entries are fully checked against remote object size and BLAKE3 before being trusted. Restore resume state now includes version/size/timestamp metadata and re-hashes every previously completed data range before skipping it. Invalid resume entries are discarded automatically.

The central goal is to detect missing or corrupt shards and restore redundancy without manual reconstruction steps. The archive manifest format remains v2.

### v0.5.2 path-sensitive remote roots

Provider-specific default paths are stored separately from archive metadata. Bare remote bases can resolve to configured paths such as `Instance:/data/crypt`, while explicit paths continue to take precedence. Capacity resolution preserves paths already specified by rclone virtual backends instead of collapsing every backing remote to its root.


## GUI redesign track

**Status: Patches 1–6 implemented through v0.5.10.**

The GUI is being reorganized before v0.6 backend work so additional storage features do not increase top-level UI complexity. The detailed staged plan is maintained in `GUI_REDESIGN.md`. Patch 1 established internal state/navigation/theme ownership; Patch 2 consolidated the sidebar into six work areas; Patch 3 centralized the restrained desktop theme and reusable status/capacity/header/toolbar widgets; Patch 4 rebuilt Dashboard around capacity, files, health, pools, warnings, and recent jobs while preserving backend behavior; Patch 5 rebuilt Files around the local inventory with search, sorting, filters, selection, details, and manifest-driven Restore / Verify / Status transitions; Patch 6 simplified Upload around storage pools, multi-file drag-and-drop queues, collapsed advanced controls, and shared preflight validation.

## v0.6 — lifecycle and optimization

**Status: planned.**

- rebalance
- enforceable failure-domain policies
- snapshots/versioning
- deduplication foundations

Metadata migrations must be added before format changes that require them.

## v0.7+ — optimization and automation

**Status: planned.**

- cost-aware placement
- bandwidth/API-rate scheduling
- storage growth analytics
- maintenance scheduling and policy automation

## Long-term invariants

- A catalog/database may accelerate lookup but must be rebuildable from manifests.
- A single cloud/provider must never be treated as the only metadata authority.
- Corruption detection is checksum-based; missing/corrupt shards are erasures for recovery purposes.
- Repair and migration should preserve the configured failure-domain policy throughout the operation where possible.
- Destructive maintenance operations should provide a dry-run mode before deletion is added.

## GUI redesign maintenance releases

- v0.5.13 completes the Jobs / Progress redesign block (A1–A4): unified job state, structured progress, Jobs running/queue/history views, expandable details, and safe in-session retry.


## Current GUI work

- v0.5.14 completes the Storage UI block (B1–B4): provider list/details, pool capacity/failure-domain analysis, and validated provider migration.


## v0.5.15 — Maintenance UI integration

- [x] C1 — Integrity overview and persistent last-check summary
- [x] C2 — Scrub workflow redesign
- [x] C3 — Repair workflow linked to scrub results
- [x] C4 — Metadata and diagnostics integration

### v0.5.15 — Crypt Secret Portability checkpoint

- [x] B1 — strict portable/secret model boundary
- [x] B2 — in-memory rclone crypt secret extraction
- [x] B3 — streaming age vault creation
- [x] B4 — exact `--no-obscure` restore
- [x] B5 — transaction, rollback, and interrupted recovery
- [x] B6 — real-tool verification harness written (runtime execution pending)
- [x] B7 — top-level `rpool export` / `rpool import` orchestration

The original version reservation at this checkpoint is historical; it is superseded by docs/VERSIONING.md.

## Future work — remaining Settings / UI Persistence

**Status: remaining GUI scope; no version reserved.** Encryption defaults and their persistence are included in 0.6.0; the broader items below are not all completed.

- D1 — Settings categories: General / Storage / Defaults / Advanced
- D2 — Storage settings including rclone executable and remote-root/provider settings
- D3 — Default pool/workers/retries/start-page defaults
- D4 — machine-local UI state persistence separated from portable configuration

Choose the next bounded GUI or backend task from user priorities; assign its version only when a validated batch is ready.
