# Transport & I/O Architecture Audit (R1–R3)

Date: 2026-09-23  
Scope: Main Update 1, Step 0/1, transport lane.  
Method: static source inspection only. No binary, Cargo command, rclone, age, age-keygen, cloud operation, or runtime test was executed. Accordingly, every behavioral statement below is **source-observed**, not runtime-verified. The checkout has no `.git` metadata at the supplied repository root, so source locations—not Git history—are the evidence base.

## 1. Current function-level transport call map

### 1.1 Composition and entry points

`main` calls `application::run`; Clap creates `Cli`, whose global `cli.rclone` string is passed unchanged by `dispatch` into command or GUI entry points (`src/application.rs:8-16`, `src/application.rs:28-30`). The CLI branches pass `&cli.rclone` to put/get/verify/status/usage/config package/pool/manifest/inventory/scrub/repair/provider/doctor paths (`src/application.rs:31-98`, `src/application.rs:103-167`, `src/application.rs:177-219`). There is no backend object or registry: the executable path is dependency injection by raw `&str` all the way down.

The default no-command branch and explicit `gui` branch pass `&cli.rclone` to `gui::launch` (`src/application.rs:11-13`, `src/application.rs:30`). GUI task execution is indirect: the GUI starts an `rpool` child, sets `RPOOL_PROGRESS_PROTOCOL=1`, and the child re-enters the CLI dispatch; this is an orchestration process boundary, not a storage backend (`src/gui/task_runner.rs:185-246`). GUI usage refresh is the exception: it directly calls storage administration helpers in a worker thread, including `list_capacity_remotes`, `list_crypt_remotes`, and `collect_quota_reports` (`src/gui/usage_refresh.rs:25-43`).

### 1.2 Data write paths to subprocesses

* **Put data:** `application::dispatch` → `commands::put` (`src/application.rs:31-65`) → `upload_one_data_shard` inside a Rayon pool (`src/commands/put.rs:126-139`) → `remote_stat_size` and, unless skipped, `upload_range` (`src/storage/data_upload.rs:6-17`) → crypt gate `ensure_crypt_destination` (`src/storage/upload.rs:12`) → `ProcessCommand::new(rclone).arg("rcat")` with streamed source range (`src/storage/upload.rs:16-50`). `hash_file_range` independently streams the local source only on the skip path (`src/storage/data_upload.rs:11-14`; `src/utils/hash.rs:3-19`).
* **Put parity:** `commands::put` → `upload_parity_groups` (`src/commands/put.rs:155-167`) → Rayon per generated parity object (`src/erasure/encode.rs:33-67`) → `remote_stat_size`; unless skipped, `upload_local_file` (`src/erasure/encode.rs:46-58`) → crypt gate → rclone `rcat` (`src/storage/upload.rs:71-124`). Parity generation itself is local, striped I/O (`src/erasure/encode.rs:119-166`).
* **Manifest replicas:** `commands::put` → `replicate_manifest` (`src/commands/put.rs:187-193`); also manifest-replicate command and provider drain call it. `replicate_manifest` calls `ensure_crypt_destinations` then `rcat_bytes` (`src/manifest/replicate.rs:19-24`) → crypt gate → rclone `rcat` (`src/storage/upload.rs:134-159`). This intentionally materializes small manifest JSON bytes, unlike shard transfer.
* **Repair writes:** scrub/repair CLI → `scan_manifest` → `repair_manifest[_filtered]` (`src/commands/scrub.rs:25-47`; `src/maintenance/repair.rs:8-29`) → `repair_group` → full-hash repaired local file and `upload_local_file` (`src/maintenance/repair.rs:230-243`) → crypt gate → rclone `rcat`.
* **Provider migration:** provider drain command → `drain_manifest` → crypt destination checks (`src/provider/migrate.rs:8-27`) → Rayon `copy_remote_object` then `verify_shard_full` (`src/provider/migrate.rs:83-98`) → rclone `copyto` (`src/storage/copy.rs:4-23`) and rclone `cat` (`src/storage/verify.rs:12-43`). It then saves the updated local manifest, replicates it, and only afterward optionally calls `delete_remote_object` for sources (`src/provider/migrate.rs:106-135`) → rclone `deletefile` (`src/storage/copy.rs:26-38`). The order is copy → full verify → local manifest → replicas → opt-in deletion.

Every remote write helper (`upload_range`, `upload_local_file`, `rcat_bytes`, and `copy_remote_object`) invokes `ensure_crypt_destination` before its subprocess (`src/storage/upload.rs:12`, `src/storage/upload.rs:78`, `src/storage/upload.rs:135`, `src/storage/copy.rs:5`). Higher layers additionally preflight put destinations and migration (`src/commands/put.rs:30`; `src/provider/migrate.rs:23-27`). This gate must remain mandatory after injection.

### 1.3 Read, verify, status, and recovery paths

* **Manifest reads:** get/status/verify/scrub/provider commands call `load_manifest`; a remote source invokes `read_remote_bytes` (`src/manifest/load.rs:9`) → `ProcessCommand::new(rclone).args(["cat", source]).output()` (`src/storage/cat.rs:3-15`). Manifest recover and replica verification also call `read_remote_bytes` (`src/manifest/recover.rs:22`; `src/manifest/replica_verify.rs:33`). This is whole-object `Vec<u8>` loading and needs a metadata-size bound.
* **Plain get:** `commands::get` → `get_plain` (`src/commands/get.rs:8-18`) → Rayon `download_one_shard` (`src/commands/get.rs:44-53`) → rclone `cat`, streaming into the correct output offset while hashing and checking exact size (`src/storage/download.rs:4-48`).
* **Erasure get:** `commands::get` → `get_erasure` (`src/commands/get.rs:13-17`) → parallel `download_one_shard` attempts (`src/commands/get.rs:90-127`); when reconstruction is needed, `reconstruct_group` downloads usable shards via `download_shard_to_file` (`src/erasure/reconstruct.rs:7-38`) → rclone `cat` streamed to temp files (`src/storage/download.rs:72-113`) and reconstructs in stripe-sized blocks (`src/erasure/reconstruct.rs:74-127`).
* **Verify:** CLI verify → `commands::verify` (`src/application.rs:73-77`) → Rayon `verify_shard_quick` or `verify_shard_full` (`src/commands/verify.rs:11-27`). Quick calls `remote_stat_size` (`src/storage/verify.rs:4-9`); full streams rclone `cat` into BLAKE3 and exact-size checks (`src/storage/verify.rs:12-43`).
* **Status:** CLI status → `commands::status` (`src/application.rs:78-82`) → Rayon direct `remote_stat_size` calls (`src/commands/status.rs:13-34`). Optional usage invokes `capacity_remotes_for` and `collect_quota_reports` (`src/commands/status.rs:153-157`).
* **Scrub/repair:** CLI scrub/repair → `scan_manifest` (`src/commands/scrub.rs:22-25`) → Rayon `probe_shard` (`src/maintenance/scan.rs:18-29`) → private `probe_remote_size` using rclone `lsjson`, and in full mode `probe_hash` using streamed rclone `cat` (`src/storage/probe.rs:3-12`, `src/storage/probe.rs:16-46`, `src/storage/probe.rs:49-102`). Repair explicitly refuses to treat `Probe::Error` as erasure (`src/maintenance/repair.rs:52-69`).
* **Restore journal:** `commands::get` passes downloaded/reconstructed completion through restore-state validation/persistence; `src/journal/restore.rs` is local state I/O and does not spawn rclone. It stays above transfer but receives verified completion only.

### 1.4 Administrative and planning paths

* `build_upload_plan(rclone, ...)` → `assign_remotes` (`src/planning/upload_plan.rs:6-14`, `src/planning/upload_plan.rs:101`) → for free-ratio placement, `capacity_remotes_for` then `query_quota` (`src/placement/assign.rs:5-14`; `src/placement/free_ratio.rs:20-39`) → `rclone config dump` and `rclone about --json` (`src/storage/remote_config.rs:153-190`; `src/storage/quota.rs:40-43`). Round-robin placement carries the string but performs no transport.
* CLI usage and status, GUI usage refresh, provider health, and free-ratio planning call `collect_quota_reports`/`query_quota`; collection is Rayon bounded by `min(workers, remotes.len())` (`src/storage/quota.rs:26-37`).
* Pool set/manage and put preflight call `ensure_crypt_destinations`; manifest replication calls it too (`src/pool/manage.rs:12`; `src/commands/put.rs:30`; `src/manifest/replicate.rs:19`). It reaches cached or direct `rclone config dump` (`src/storage/remote_config.rs:52-80`, `src/storage/remote_config.rs:126-190`).
* Doctor calls `list_rclone_remotes`, separately probes `rclone version`, and checks crypt destinations (`src/doctor/check.rs:22-49`, `src/doctor/check.rs:125`). These are tool/config diagnostics, not object byte I/O.
* Provider health calls `check_providers` → Rayon `check_provider` → direct rclone `lsf`, plus capacity resolution and `query_quota` (`src/provider/health.rs:6-45`).

### 1.5 Free-function wrapper inventory and callers

| Wrapper | Direct callers | Subprocess endpoint |
|---|---|---|
| `read_remote_bytes` | `manifest/load.rs`, `manifest/recover.rs`, `manifest/replica_verify.rs` | `rclone cat` via captured `Output` |
| `remote_stat_size` | `storage/data_upload.rs`, `storage/verify.rs::verify_shard_quick`, `commands/status.rs`, `erasure/encode.rs` | `rclone lsjson <object> --stat` |
| `probe_shard` | only `maintenance/scan.rs::scan_manifest` | private `lsjson`; optional streamed `cat` |
| `upload_range` | only `storage/data_upload.rs::upload_one_data_shard` | crypt gate; streamed `rclone rcat --size` |
| `upload_local_file` | `erasure/encode.rs`, `maintenance/repair.rs` | crypt gate; streamed `rclone rcat --size` |
| `rcat_bytes` | only `manifest/replicate.rs` | crypt gate; `rclone rcat` |
| `download_one_shard` | `commands/get.rs` plain and erasure attempts | streamed `rclone cat` into ranged output |
| `download_shard_to_file` | `erasure/reconstruct.rs`, `maintenance/repair.rs` | streamed `rclone cat` into temp file |
| `verify_shard_quick` | only `commands/verify.rs` | delegates to `remote_stat_size` |
| `verify_shard_full` | `commands/verify.rs`, `journal/upload.rs`, `provider/migrate.rs` | streamed `rclone cat` + size/BLAKE3 |
| `copy_remote_object` | only `provider/migrate.rs` | crypt gate; `rclone copyto` |
| `delete_remote_object` | only `provider/migrate.rs` | `rclone deletefile` |
| `list_rclone_remotes` | `doctor/check.rs` | `rclone listremotes` |
| `list/capacity/crypt remotes`, `ensure_crypt_*` | GUI usage, usage/status, placement, pool, put, manifest replicate, migration, doctor | `rclone config dump` (cached only in gate path) |
| `collect_quota_reports` / `query_quota` | usage/status, GUI usage, provider health, placement | `rclone about --json` |

## 2. R1 confirmation — stat error collapse

**Confirmed from source.** `remote_stat_size` spawns `rclone lsjson`; process-spawn failure is an `Err`, but **every successfully spawned process with a non-success exit status returns `Ok(None)` without examining stderr or exit class** (`src/storage/stat.rs:3-10`). Therefore authentication denial, permission denial, timeout reported by rclone, throttling/rate limit, provider outage, malformed remote, and true not-found all collapse to absence. Directory and absent `Size` also become `None` (`src/storage/stat.rs:13-18`).

By contrast, private `probe_remote_size` examines stderr and returns `Ok(None)` only for six English missing substrings; other nonzero exits become errors (`src/storage/probe.rs:22-38`). `probe_shard` maps `Ok(None)` to `Probe::Missing` and errors to `Probe::Error` (`src/storage/probe.rs:3-13`). This distinction is imperfect (string/locale dependent) but it is materially safer than `remote_stat_size`.

**Every `remote_stat_size` caller and concrete consequence:** 

1. `upload_one_data_shard` (`src/storage/data_upload.rs:11`): any nonzero rclone exit appears `None`, so it attempts an upload. An auth/permission failure is not itself mistaken for successful upload, but a transient stat failure can trigger an unnecessary overwrite attempt; where a later layer changes credentials/state, the decision was made from false “absent” information.
2. `upload_parity_groups` (`src/erasure/encode.rs:46`): same false-absence behavior and unnecessary overwrite attempt for parity.
3. `verify_shard_quick` (`src/storage/verify.rs:5-9`), called by `commands::verify` (`src/commands/verify.rs:20-24`): auth/rate-limit/timeout/provider failure is reported as “missing shard”, destroying the operational distinction.
4. `commands::status` (`src/commands/status.rs:22-30`): all such failures increment `missing`, not `errors`; group availability/recoverability calculations then consume this false state (`src/commands/status.rs:104-120`).

**Every `probe_shard` caller:** only `scan_manifest` (`src/maintenance/scan.rs:8-29`), reached by scrub and repair (`src/commands/scrub.rs:25`, `src/commands/scrub.rs:44-47` and repair command). Its `Probe::Error` count is preserved (`src/maintenance/scan.rs:37-44`), and repair refuses mutation when selected transport/provider errors exist (`src/maintenance/repair.rs:52-59`). Thus the explicit “auth failure treated as Missing → repair proceeds” risk exists in the generic stat contract and status/quick verify classification, but the currently observed automatic repair path uses `probe_shard` and has a fail-closed guard. The immediate write-path R1 effect is false absence causing upload/overwrite attempts, not evidence that repair currently bypasses that guard.

Required correction: one typed stat implementation must classify `NotFound` separately and propagate `Authentication`, `PermissionDenied`, `RateLimited`, `Timeout`, and unknown failures. Both status and maintenance must consume the same classification; unknown must remain unknown/error.

## 3. R2 confirmation — stale journal object re-adoption

**Confirmed exact flow:** 

1. `commands::put` loads/creates the journal, then calls `validate_upload_journal` before deriving incomplete data indexes (`src/commands/put.rs:98-109`).
2. For every completed journal shard with matching object/size/kind metadata, `validate_upload_journal` calls `verify_shard_full` (`src/journal/upload.rs:30-41`).
3. `verify_shard_full` streams the current remote logical bytes through BLAKE3 and validates exact size and the journal's expected digest; a rclone failure, size mismatch, or digest mismatch returns `Err` (`src/storage/verify.rs:12-43`).
4. The journal validator catches **all** such errors with `.is_err()`, pushes the index, and removes that completed entry (`src/journal/upload.rs:41-47`). The reason is discarded.
5. `commands::put` saves the invalidated journal and includes that data plan shard in `data_plan` (`src/commands/put.rs:99-110`), then invokes `upload_one_data_shard` (`src/commands/put.rs:126-139`).
6. `upload_one_data_shard` immediately calls `remote_stat_size`; if the remote object has the planned size, it hashes the **local source**, prints `[skip]`, constructs a manifest shard from that local hash, and performs no remote-byte verification or write (`src/storage/data_upload.rs:11-17`). It then records that shard completed (`src/commands/put.rs:135-136`).
7. For parity, `commands::put` calls `upload_parity_groups` after data (`src/commands/put.rs:155-167`). Generated parity has its locally computed BLAKE3, but if the existing remote object has the same size, it is skipped and `shard_from_plan` adopts the local expected hash without checking remote bytes (`src/erasure/encode.rs:40-64`). That returned shard is recorded in the journal (`src/commands/put.rs:164-166`).

**Precise regression scenario:** an object at the same legacy address contains bytes different from the current local shard but has exactly the expected length—for example same-length corruption, an interrupted/incorrect prior overwrite that preserved length, or changed source content under a reused plan/address. Full journal validation correctly detects the BLAKE3 mismatch and removes the entry. The subsequent size-only check sees `Some(expected_size)`, skips upload, and recreates a `Shard` carrying the local expected BLAKE3. The same corrupt/different remote bytes are therefore re-adopted as completed. Data follows steps 5–6; parity follows step 7. A transient full-verify error can also invalidate then size-skip if the later stat succeeds, so unverified bytes can be adopted even without proof of corruption.

Required correction: reusable-object logic belongs in `transfer` and must verify logical-byte size **and expected BLAKE3** before returning reusable. Any ambiguous transport error fails closed. Journal completion is recorded only after verified remote completion. The expected digest must be available before reuse decision; changed local source/plan state must not silently bless old bytes.

## 4. R3 subprocess classification and target boundary

| Current call | Classification | Why | Target boundary |
|---|---|---|---|
| Object `cat`, `lsjson --stat`, `rcat`, `copyto`, `deletefile` in `src/storage/*` | Genuine storage I/O | Operates on archive object logical bytes/metadata | `storage/backends/rclone` for primitive operations; hash/retry/copy orchestration in `transfer` |
| `provider/health.rs` direct `rclone lsf` (`src/provider/health.rs:8-20`) | Backend administration | Accessibility/list probe is provider health/admin, not a byte-stream trait concern | rclone implementation under `storage/admin`; provider domain retains health policy |
| `quota.rs` `about/listremotes` and `remote_config.rs` `config dump` | Backend administration/config | Capacity, discovery, alias resolution, and write-policy evidence are not generic object read/write | `storage/admin`, with an explicit crypt-policy/config context |
| `doctor/check.rs` `rclone version` (`src/doctor/check.rs:49`) | Tool diagnostic | Tests external executable availability/version, not remote object semantics | dedicated tool-diagnostic boundary; do **not** add to `StorageBackend` |
| `config_sync/*` rclone config and age/age-keygen subprocesses | Secret wrapper | Owns sanitized argv/stdin/stdout, vault encryption, transactional config restoration, and secret redaction | keep in `config_sync` secret-process/tooling boundary; never route secret bytes through generic storage APIs |
| `gui/task_runner.rs` rpool child (`src/gui/task_runner.rs:185-246`) | GUI process orchestration | Executes the application CLI and consumes structured progress/cancellation; it is not a cloud adapter | keep GUI task/process boundary and `RPOOL_PROGRESS_PROTOCOL=1` |

R3 is therefore confirmed: the direct health `lsf` call is outside storage today, but “move every subprocess into StorageBackend” would be wrong. The correct split is data backend, backend admin, tool diagnostics, secret wrapper, and GUI child process.

## 5. Storage-layer KEEP / REFACTOR / REPLACE / REMOVE map

“Remove” below means remove the duplicate owning implementation after callers migrate, not delete behavior prematurely.

| File | Current role | Decision | Reason | Target module |
|---|---|---|---|---|
| `storage/mod.rs` | Flat re-export facade | REFACTOR | Become module boundary for traits, registry, backends, admin; temporary wrappers only | `storage/mod.rs`, `storage/traits`, `storage/registry` |
| `storage/cat.rs` | Whole remote object to `Vec<u8>` | REPLACE | Primitive belongs to adapter; convenience metadata read needs a hard size limit | `storage/backends/rclone` + bounded helper in `transfer` |
| `storage/stat.rs` | Size-only stat with error collapse | REMOVE after bridge migration | Duplicates probe logic and violates typed-error invariant (R1) | unified `StorageBackend::stat` in adapter |
| `storage/probe.rs` | Stat/full hash and health policy mixed | REFACTOR | Keep probe result policy, replace subprocesses with backend + transfer verification | `transfer/verify`; maintenance consumes result |
| `storage/copy.rs` | rclone copy/delete plus retry/gate | REFACTOR | Adapter owns native copy/delete; transfer owns cross-backend copy/retry; policy owns deletion order | `storage/backends/rclone`, `transfer`, provider policy |
| `storage/upload.rs` | Range/file/bytes rcat, retry, hash, crypt check | REFACTOR | Separate primitive write, replayable source, retry/hash, and mandatory write authorization | adapter + `transfer/upload` + security gate |
| `storage/download.rs` | Download, retry, hash, direct output writes | REFACTOR | Preserve streaming and offset semantics; inject backend and centralize verified completion | `transfer/download` |
| `storage/verify.rs` | Quick stat and full BLAKE3 | REFACTOR | One backend-neutral verification service; eliminate quick stat's R1 behavior | `transfer/verify` |
| `storage/data_upload.rs` | Resume skip and data upload policy | REPLACE | R2 size-only reuse is unsafe; common verified-reuse service required | `transfer/upload` or archive upload service |
| `storage/quota.rs` | remote discovery/capacity subprocesses | REFACTOR | Administrative capability, not core byte trait; keep bounded collection | `storage/admin/rclone` |
| `storage/remote_config.rs` | config dump, capacity aliasing, crypt gate/cache | REFACTOR | Preserve crypt evidence, but split admin/config lookup from write authorization; cache key cannot be executable string alone | `storage/admin/rclone` + security/write policy |
| `utils/hash.rs` | Streaming local range BLAKE3 | KEEP and generalize carefully | Correct bounded streaming primitive; useful to transfer sources | `transfer/hash` or retained utility |
| `utils/file_io.rs` | positional exact file I/O | KEEP | Preserves concurrent offset semantics for restore/reconstruct | utility used by transfer/erasure |

No storage behavior should be deleted until all old callers use the injected service. At Step 5 completion, legacy free functions may remain only as thin transition bridges to the single adapter/transfer implementation (`MASTER_PROMPT:618-620`).

## 6. Concurrency, retry, memory, deadline, and cancellation

* **Rayon fan-out:** put data uses `workers` concurrent objects (`src/commands/put.rs:126-139`); parity uses another `workers` pool (`src/erasure/encode.rs:33-67`); get, verify, status, scan, migration, quota, and provider health similarly construct local pools (`src/commands/get.rs:44-53`, `src/commands/verify.rs:11-27`, `src/commands/status.rs:13-34`, `src/maintenance/scan.rs:18-29`, `src/provider/migrate.rs:83-98`, `src/storage/quota.rs:26-35`, `src/provider/health.rs:37-43`). A backend that adds its own request pool can multiply concurrency. The backend registry/service needs a shared per-backend semaphore/budget rather than one limiter per wrapper or command.
* **Retry multiplication:** upload/download/copy helpers each interpret `retries.max(1)` as total attempts (`src/storage/upload.rs:13-16`, `src/storage/download.rs:5-8`, `src/storage/copy.rs:6-9`). If the new adapter also retries, total calls become upper-layer attempts × adapter attempts. Choose one retry owner (normally transfer), and let adapter return typed outcomes/retry hints. Native rclone itself may internally retry too, so explicit rclone flags/default semantics must be documented.
* **Replay safety:** current uploads reopen and reseek the local file each attempt (`src/storage/upload.rs:16-33`, `src/storage/upload.rs:82-98`), which is replayable but does not prove the file stayed unchanged between attempts. A replayable-source contract should pin/validate identity, size, and expected hash or fail if changed.
* **Streaming strengths:** shard upload/download/probe/full verify use `IO_BUFFER` loops; encode/reconstruct/repair use stripe-sized blocks (`src/storage/upload.rs:33-45`; `src/storage/download.rs:22-44`; `src/storage/probe.rs:64-75`; `src/storage/verify.rs:20-31`; `src/erasure/encode.rs:127-160`; `src/maintenance/repair.rs:159-223`). Preserve these paths.
* **Whole-object memory:** `read_remote_bytes` captures all stdout in `Vec<u8>` (`src/storage/cat.rs:3-15`), acceptable only for explicitly bounded metadata. `rcat_bytes` similarly accepts all bytes already resident (`src/storage/upload.rs:134`). Reed-Solomon allocates `(data+parity) × stripe_size` vectors per stripe, not full shards (`src/erasure/encode.rs:127-132`; `src/maintenance/repair.rs:159-165`).
* **Deadline/cancel boundary:** subprocess calls have no explicit deadline. Streaming loops block on child pipes; cancellation cannot reliably interrupt them. Put/get/verify/status/scrub/migration need an `OperationContext` propagated to transfer and adapter, with deadline/cancel checks, kill-and-reap behavior, and an `UnknownOutcome` write result when cancellation/timeout occurs after remote acceptance. GUI child cancellation remains at its process boundary and must not be advertised as immediate cancellation of an already-blocked descendant I/O.
* **Partial output:** download-to-output writes before final verification (`src/storage/download.rs:22-44`); state is marked only after helper success, but failed bytes can remain in the output range. Transfer must distinguish partial sink mutation from verified completion and either overwrite on retry or stage/publish safely.
* **Pipe lifecycle:** adapter ownership must cover stdin/stdout closure, concurrent stderr handling, early errors, child kill/reap, and avoid waiting while an unread pipe can fill. The current shard streams inherit stderr, while captured-output helpers buffer both streams; preserve bounded behavior and sanitized errors.

## 7. Step 2/5/6/7 touch order and write-security gate

Exact existing files and planned new owning files, in dependency order:

1. **Step 2 contract first:** add `src/storage/traits.rs`, `capabilities.rs`, `error.rs`, `reference.rs`, `registry.rs`; update `src/storage/mod.rs`; add the small owning model modules for `BackendId`, object/reference types and volume identifiers (expected `src/models/storage/*` and `src/models/volume/*`, plus their `mod.rs` exports). Do not change default command routing yet (`MASTER_PROMPT:533-548`).
2. **Step 5 adapter/admin/transfer:** add `src/storage/backends/mod.rs`, `src/storage/backends/rclone/*`, `src/storage/admin/*`, and `src/transfer/*`; update `src/storage/{cat,stat,probe,copy,upload,download,quota,remote_config,verify,data_upload,mod}.rs` as bridges or remove their duplicate internals only after migration. Update `src/provider/health.rs` for admin injection. Keep `src/doctor/check.rs`, `src/config_sync/*`, and `src/gui/task_runner.rs` in their classified non-storage boundaries.
3. **Step 6 read injection:** update `src/application.rs` composition, then `src/commands/{get,status,verify,scrub}.rs`, `src/manifest/{load,recover,replica_verify}.rs`, `src/maintenance/scan.rs`, `src/erasure/reconstruct.rs`, and `src/journal/restore.rs` (`MASTER_PROMPT:624-645`). Preserve output offsets, v1/v2 manifest strings, and error distinctions.
4. **Step 7 write injection:** update `src/commands/put.rs`, `src/storage/data_upload.rs` (or retire into transfer), `src/erasure/encode.rs`, `src/journal/upload.rs`, `src/manifest/replicate.rs`, `src/maintenance/repair.rs`, and `src/provider/migrate.rs` (`MASTER_PROMPT:649-665`). Then update any remaining pool/usage/GUI admin callers to receive registry/admin services rather than a raw executable string.

**Mandatory security gate:** every shard, parity, repaired shard, migrated destination, and manifest-replica write must pass a verified encryption binding before bytes are sent. For the legacy rclone path this means preserving the effective `ensure_crypt_destination` policy: destination must resolve to a configured `crypt` remote with `no_data_encryption != true` (`src/storage/remote_config.rs:52-69`). The gate must sit on the common authorized write path so no backend primitive, retry, native copy, manifest helper, repair, or migration can bypass it. Raw backing remotes may be used for capacity administration, never as legacy archive write destinations. Different crypt aliases/keys must transfer logical bytes and verify them; ciphertext copying across raw backings is forbidden unless encryption mapping identity is proven.

## 8. Ten-line summary

1. Static inspection only: no build, test, binary, tool, or cloud operation was performed.
2. Raw `rclone: &str` currently flows from CLI/GUI composition through domain helpers to each subprocess.
3. R1 is confirmed: `remote_stat_size` converts every nonzero rclone exit into `Ok(None)`.
4. Status and quick verify therefore misreport auth, timeout, rate-limit, and provider failures as missing.
5. Scrub/repair uses the safer `probe_shard` path and presently refuses repair on `Probe::Error`.
6. R2 is confirmed: full journal verification can invalidate an object that data/parity code immediately re-adopts by size alone.
7. Top risk 1: same-size corrupt or different bytes can be recorded and manifested as successfully completed.
8. Top risk 2: collapsed transport errors can drive false absence, misleading health, and unnecessary overwrite attempts.
9. Top risk 3: Rayon × adapter × rclone retries/concurrency can multiply requests without a shared bound or deadline.
10. The pivot must centralize typed I/O and verified transfer while preserving the crypt write gate and non-storage subprocess boundaries.
