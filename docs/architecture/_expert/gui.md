# GUI, Progress, Secret-Portability, Doctor, and Cleanup Architecture Audit

## Scope and evidence level

This report is a **static source audit** of the requested checkout. All findings marked “observed” come from reading actual files. Nothing is runtime-verified: no Cargo command, RPool binary, rclone/age tool, cloud account, or secret material was used. No build/test success is claimed.

## 1. GUI child-process and progress architecture

### Observed structure

1. With no CLI subcommand, `application::run` launches the GUI; an explicit `gui` command follows the same path. Other commands are dispatched in-process by the child invocation (`src/application.rs:8-16`, `src/application.rs:28-31`).
2. GUI screens build command arguments and call the active `crate::gui::task::TaskRunner`; for example provider health/drain use that runner (`src/gui/screens/storage/providers.rs:78-89`, `src/gui/screens/storage/providers.rs:92-125`). The active module is `src/gui/task/runner.rs`, selected by `mod task` and `mod runner` (`src/gui/mod.rs:4-6`, `src/gui/task/mod.rs:1-6`).
3. `TaskRunner::start_rpool` rejects overlap, resolves the current `rpool` executable, prepends `--rclone <configured executable>`, stores an in-memory invocation for retry, and starts a worker thread (`src/gui/task/runner.rs:53-101`). The worker—not the GUI thread—spawns the child (`src/gui/task/runner.rs:253-277`).
4. The child receives `RPOOL_PROGRESS_PROTOCOL=1`; stdin is null and stdout/stderr are independently piped (`src/gui/task/runner.rs:259-265`). Separate reader threads prevent either pipe from blocking the other (`src/gui/task/runner.rs:279-286`).
5. The machine protocol is newline-framed stderr: prefix `@rpool-progress ` followed by tagged JSON events `start`, `advance`, `items`, or `finish` (`src/progress/protocol.rs:3-24`). Emission is conditional on the environment variable, serialized under a mutex, and written to stderr (`src/progress/emitter.rs:4-25`). Only stderr is inspected for protocol records; recognized records become progress events and are omitted from human logs, while stdout and non-protocol stderr remain logs (`src/gui/task/runner.rs:370-399`).
6. The GUI polls channels without blocking its event loop and requests periodic repaint while background work exists (`src/gui/app.rs:44-63`, `src/gui/app.rs:66-72`). Progress events update byte/item state in the GUI-side tracker (`src/gui/task/progress.rs:17-80`).
7. Cancellation sets an atomic flag; the worker notices it, terminates the child process tree (Unix process group / Windows `taskkill`, then child-kill fallback), drains both output threads, and reports a cancelled outcome (`src/gui/task/runner.rs:189-193`, `src/gui/task/runner.rs:288-320`, `src/gui/task/runner.rs:323-368`).
8. Retry metadata is session-local in `last_task.invocation`; retry reconstructs a fresh child invocation rather than using persisted history (`src/gui/task/model.rs:75-105`, `src/gui/task/runner.rs:181-186`, `src/gui/task/runner.rs:235-250`). Persisted CLI task history is descriptive and redacted before append: GUI/history commands are not themselves recorded, targets and error messages pass through redaction (`src/history/track.rs:14-17`, `src/history/track.rs:81-102`), and redaction masks common secret tokens and URL credentials/query/fragment (`src/history/redact.rs:1-8`, `src/history/redact.rs:11-48`).

### Preservation requirements

- Preserve the GUI → current `rpool` child boundary, `--rclone` compatibility argument, `RPOOL_PROGRESS_PROTOCOL=1`, and structured stderr protocol. Do not replace it with parsing human prose.
- Preserve independent stdout/stderr draining, protocol-line suppression from human logs, start-failure reporting, exit-code/outcome mapping, cancellation and process-tree cleanup.
- Preserve retry as validated in-memory/session-local invocation data; persisted history must remain display/audit data, never an executable command source.
- Preserve history redaction. **Risk:** the generic GUI runner currently formats all arguments into a visible command preview (`src/gui/task/runner.rs:73-89`, `src/gui/task/runner.rs:402-416`). Secret-bearing portability/config operations therefore must remain outside this generic runner unless argument classification/redaction is added first.
- Storage I/O must never execute on the GUI thread. Existing long tasks use a worker child, and usage refresh uses a background thread (`src/gui/usage_refresh.rs:25-53`). Backend-neutral migration work must retain an equivalent worker/cancellation/backpressure boundary.
- Cancellation is cooperative at the GUI-worker boundary and forceful at the child boundary; it must not be represented as proof that an already-blocked backend request stopped instantaneously.

## 2. R5 confirmation: cleanup-list reachability

Method: each explicit path at `scripts/update-cleanup.nu:19-42` was checked for existence and against all Rust `mod`, `#[path]`, and re-export declarations. `DEAD` means not reachable from the crate module tree; for already-absent entries it means “absent and no declaration resolves to this path.” No entry is `ACTIVE-via-path` or `AMBIGUOUS` in this checkout.

| Listed path | Verdict | Static evidence |
|---|---|---|
| `src/models/sync_config.rs` | **DEAD** (present) | `src/models/mod.rs:1-19` declares no `sync_config`; portable config is instead `mod portable_config` at line 16 and re-exported at lines 36-39. |
| `src/gui/state.rs` | **DEAD** (absent) | `src/gui/mod.rs:4-5` explicitly maps `state` to `state/mod.rs`. |
| `src/gui/widgets/usage_card.rs` | **DEAD** (absent) | Complete widget declarations are in `src/gui/widgets/mod.rs:1-7`; no `usage_card`. |
| `src/gui/screens/dashboard.rs` | **DEAD** (absent) | `src/gui/screens/mod.rs:1-2` explicitly maps dashboard to `dashboard/mod.rs`. |
| `src/gui/screens/maintenance.rs` | **DEAD** (absent) | `src/gui/screens/mod.rs:5-6` explicitly maps maintenance to `maintenance/mod.rs`. |
| `src/gui/screens/integrity.rs` | **DEAD** (absent) | Top-level screen declarations are `src/gui/screens/mod.rs:1-8`; integrity is nested and explicitly mapped by `src/gui/screens/maintenance/mod.rs:1-2`. |
| `src/gui/screens/manifest.rs` | **DEAD** (absent) | No top-level manifest module (`src/gui/screens/mod.rs:1-8`); active metadata manifest is declared by `src/gui/screens/maintenance/metadata/mod.rs:1-2`. |
| `src/gui/screens/pools.rs` | **DEAD** (absent) | Pools is nested under active `storage` (`src/gui/screens/mod.rs:7-8`; `src/gui/screens/storage/mod.rs:1-2`). |
| `src/gui/screens/providers.rs` | **DEAD** (absent) | Providers is nested under active `storage` (`src/gui/screens/mod.rs:7-8`; `src/gui/screens/storage/mod.rs:1-2`). |
| `src/gui/screens/storage/providers/actions.rs` | **DEAD** (absent) | `providers` is the flat active file `src/gui/screens/storage/providers.rs`, selected by `src/gui/screens/storage/mod.rs:2`; it declares no child `actions`. |
| `src/gui/screens/restore.rs` | **DEAD** (absent) | Active restore is nested under files (`src/gui/screens/files/mod.rs:3`). |
| `src/gui/screens/status.rs` | **DEAD** (absent) | Active status is nested under files (`src/gui/screens/files/mod.rs:4`). |
| `src/gui/screens/upload.rs` | **DEAD** (absent) | Active upload is explicitly `files/upload/mod.rs` (`src/gui/screens/files/mod.rs:5-6`). |
| `src/gui/screens/verify.rs` | **DEAD** (absent) | Active verify is nested under files (`src/gui/screens/files/mod.rs:7`). |
| `src/gui/screens/files/inventory.rs` | **DEAD** (absent) | Explicit mapping selects `files/inventory/mod.rs` (`src/gui/screens/files/mod.rs:1-2`). |
| `src/gui/screens/files/upload.rs` | **DEAD** (absent) | Explicit mapping selects `files/upload/mod.rs` (`src/gui/screens/files/mod.rs:5-6`). |
| `src/gui/task_runner.rs` | **DEAD** (present duplicate) | `src/gui/mod.rs:6` declares only `task`; `src/gui/task/mod.rs:1-6` declares/re-exports `task/runner.rs`. There is no `mod task_runner` or path override. References inside other dead duplicates do not make it reachable. |
| `src/gui/screens/maintenance/integrity.rs` | **DEAD** (present duplicate) | `src/gui/screens/maintenance/mod.rs:1-2` explicitly selects `integrity/mod.rs`, whose live children are listed at `src/gui/screens/maintenance/integrity/mod.rs:1-6`. |
| `src/gui/screens/maintenance/manifest.rs` | **DEAD** (present duplicate) | Maintenance exports `metadata::ManifestForm` (`src/gui/screens/maintenance/mod.rs:3-9`); `metadata/mod.rs` declares its own `manifest` child (`src/gui/screens/maintenance/metadata/mod.rs:1-2`). No declaration selects this flat duplicate. |
| `src/gui/screens/maintenance/system.rs` | **DEAD** (present duplicate) | Active module is `diagnostics` and `SystemForm` is re-exported from it (`src/gui/screens/maintenance/mod.rs:5-9`); no `mod system`. |
| `src/gui/screens/maintenance/integrity/controls.rs` | **DEAD** (absent) | Active integrity children are exactly repair/result/scrub/state/summary/target (`src/gui/screens/maintenance/integrity/mod.rs:1-6`). |
| `src/gui/screens/maintenance/integrity/repair_legacy.rs` | **DEAD** (absent) | Active integrity module declares `repair`, not `repair_legacy` (`src/gui/screens/maintenance/integrity/mod.rs:1-6`). |

The four present dead files (`models/sync_config.rs`, `gui/task_runner.rs`, and the three flat maintenance duplicates—five files total) are legitimate Step 11 removal candidates after a final same-commit module/caller search. The 17 absent entries remain useful explicit cleanup targets for upgrading older installations. The cleanup script itself verifies `Cargo.toml` exists and contains `name = "rpool"` (`scripts/update-cleanup.nu:5-15`), operates only on its literal list (`scripts/update-cleanup.nu:17-42`), and retains `--dry-run` behavior (`scripts/update-cleanup.nu:46-62`).

## 3. Secret-portability boundary

### Secret/config wrappers — must **not** become `StorageBackend`

- **age vault wrapper:** `AgeEncrypt`/`AgeDecrypt` spawn age to stream an encrypted vault or authenticated decrypted bytes; identity must be outside the artifact root (`src/config_sync/age_vault.rs:23-68`, `src/config_sync/age_vault.rs:71-99`). This is secret-envelope/config portability, not object storage.
- **rclone obscure wrapper:** new-remote generation gets OS randomness, sends it through `rclone obscure -`, and captures bounded sensitive output (`src/config_sync/crypt_generate.rs:32-48`). This transforms secret representation; it does not transfer archive objects.
- **rclone config wrappers:** `config dump` extracts only crypt structure/password fields (`src/config_sync/crypt_secrets.rs:41-76`); restore uses `config update` against a transaction-owned staged config and verifies exact values (`src/config_sync/crypt_restore.rs:54-92`). `config file` only discovers the local config path (`src/config_sync/tooling.rs:18-34`). These are provider/tool configuration operations, not storage data I/O.
- **age-keygen wrapper:** `age-keygen -y` derives a recipient from an external identity (`src/config_sync/tooling.rs:44-55`); it is also a secret/tooling boundary.
- All secret-bearing subprocesses use the dedicated wrapper with bounded output, closed stderr, deadline, generic errors, and child cleanup (`src/config_sync/secret_process.rs:1-16`, `src/config_sync/secret_process.rs:19-64`). Its rclone command sanitizes inherited `RCLONE_*` controls except config unlock and forces non-logging config use (`src/config_sync/secret_process.rs:66-78`). This security wrapper must not be generalized into transport APIs.

### Storage-I/O classification

No audited `config_sync` subprocess performs archive object read/write/list/copy/delete. Filesystem work in this module is local artifact/config transaction I/O; subprocesses are age encryption/decryption, rclone secret obscuring/config inspection/config mutation, config-path discovery, or recipient derivation. Therefore **none** belongs behind `StorageBackend`. The package exporter reads crypt config then writes the encrypted local artifact and bound portable JSON (`src/config_sync/export.rs:59-98`); that is portability packaging, not cloud storage transport.

### Generation and credential scope

- New-remote secrets use `GENERATED_SECRET_BITS = 1024`, allocate exactly `1024 / 8` bytes, and fill them via the OS RNG (`getrandom::fill`) (`src/config_sync/crypt_generate.rs:12-15`, `src/config_sync/crypt_generate.rs:32-35`). Password and password2 call `generate_one` independently (`src/config_sync/crypt_generate.rs:54-64`). Thus the policy is 1024 **bits per secret**, not 1024 characters.
- Generation refuses existing/duplicate remote names and explicitly does not create remotes or rotate keys (`src/config_sync/crypt_generate.rs:51-60`).
- Provider credentials/OAuth tokens are outside this portability model: the config dump model deliberately skips unknown credential fields (`src/config_sync/crypt_secrets.rs:11-29`), and only crypt remotes/password fields are collected (`src/config_sync/crypt_secrets.rs:57-76`). The source test fixture illustrates that other-backend credential fields are ignored (`src/config_sync/crypt_secrets.rs:85-94`), but the test was not run here.
- Portable JSON excludes crypt passwords; the encrypted `secrets/rclone.age` artifact is digest-bound (`src/config_sync/export.rs:51-61`, `src/config_sync/export.rs:92-98`).

## 4. Doctor boundary

`check_rclone` executes only `rclone version` and reports availability/version diagnostics (`src/doctor/check.rs:48-59`). It is a **tool diagnostic**, not storage I/O, and should stay outside `StorageBackend`. Step 8 should make it conditional on an rclone-backed/compatibility configuration rather than mandatory for native-only/test backends. Separately, `run_checks` currently also calls remote discovery and crypt-destination checks through storage helpers (`src/doctor/check.rs:18-37`, `src/doctor/check.rs:103-140`); those should be classified independently as provider/config diagnostics and injected backend capability checks, not conflated with the version probe.

## 5. KEEP / REFACTOR / REPLACE / REMOVE matrix

| File/layer | Current role | Decision | Reason | Target module/boundary |
|---|---|---|---|---|
| `src/gui/task/{mod,model,progress,runner}.rs` | Active child execution, progress state, cancellation/retry/logging | **KEEP + REFACTOR narrowly** | Protocol/lifecycle is a compatibility invariant; future backend-neutral task metadata may need typed/redacted previews | `gui::task`; worker-facing application operation API, not direct storage on UI thread |
| `src/progress/{mod,protocol,emitter}.rs` | Structured child→GUI stderr events | **KEEP** | Stable machine contract independent of backend | `progress` shared by command/application operations and GUI child runner |
| `src/gui/usage_refresh.rs` | Background rclone discovery/quota refresh | **REFACTOR** | Already off-thread, but directly couples GUI to rclone storage helpers | Background provider/capability query service injected into GUI |
| `src/gui/screens/storage/*` | Pool/provider administration and task argument construction | **REFACTOR** | Preserve compatibility UI; inject provider administration and distinguish physical backing/alias/domain | GUI presenter over application/provider services; no `StorageBackend` calls on GUI thread |
| `src/gui/app.rs`, `navigation.rs`, `theme.rs`, `state/*`, remaining screens/widgets | Six-area GUI shell/state/presentation | **KEEP** | No storage abstraction should force unrelated GUI redesign | Existing `gui` modules |
| `src/config_sync/age_vault.rs`, `crypt_generate.rs`, `crypt_restore.rs`, `crypt_secrets.rs`, `plaintext_config.rs`, `secret_process.rs`, `tooling.rs`, `transaction/*`, `export.rs`, `import.rs`, `validate.rs` | B1–B7 local config/secret portability | **KEEP as separate boundary** | Security semantics and subprocess hardening are not archive transport | `config_sync` / dedicated secret-tool adapters; never `StorageBackend` |
| `src/doctor/check.rs` | Tool/config/local-state/provider diagnostics | **REFACTOR** | Keep version probe as tool diagnostic; gate rclone-only checks and inject backend-aware diagnostics | `doctor` orchestration + tool diagnostic + provider/backend diagnostic adapters |
| `src/application.rs`, `src/cli/*` | GUI-vs-CLI dispatch and propagation of `--rclone` | **REFACTOR incrementally** | Preserve legacy child argv while injecting backend selection/services into command paths | Application composition root / CLI compatibility adapter |
| `src/gui/task_runner.rs` | Superseded flat runner duplicate | **REMOVE** | Unreachable; active implementation is `gui/task/runner.rs` | None; explicit cleanup list |
| `src/gui/screens/maintenance/{integrity.rs,manifest.rs,system.rs}` | Superseded flat screen duplicates | **REMOVE** | Explicit folder-path/diagnostics modules are active | `maintenance/integrity/mod.rs`, `maintenance/metadata/manifest.rs`, `maintenance/diagnostics.rs` |
| `src/models/sync_config.rs` | Superseded model | **REMOVE** | No module declaration/re-export; portable model is active | `models/portable_config.rs` |
| Already-absent obsolete-list paths | Legacy file-to-folder remnants | **KEEP listed, no source removal needed** | Supports cleanup of older deployed trees safely | Explicit `scripts/update-cleanup.nu` entries |
| `scripts/update-cleanup.nu` | Explicit stale-file cleanup | **KEEP + UPDATE explicitly** | R5 requires auditable targets and safe upgrades | Same script; literal paths only |

“REPLACE” applies only to direct GUI/provider calls that become injected application/provider services; it does **not** apply to the child progress contract or secret subprocess wrappers.

## 6. Step 8 / Step 11 touch order

### Step 8 — exact files, in recommended order

1. `src/provider/health.rs`, `src/storage/quota.rs`, `src/storage/remote_config.rs`: define the provider discovery/quota/tool-admin split and adapter-facing contracts first.
2. `src/doctor/check.rs`: separate/gate the rclone version tool diagnostic and consume injected provider/backend diagnostics.
3. `src/application.rs` and the relevant `src/cli.rs` / `src/cli/*.rs`: compose backend/provider/tool services while preserving legacy `--rclone` dispatch.
4. `src/remote_root/{mod,load,manage,resolve,save}.rs` and `src/pool/{mod,load,manage,resolve,save,targets,validate}.rs`: adapt configuration resolution only after service contracts are fixed.
5. `src/gui/usage_refresh.rs`: replace direct rclone-helper coupling with the background query service.
6. `src/gui/task/{mod.rs,model.rs,progress.rs,runner.rs}` and `src/progress/{mod.rs,protocol.rs,emitter.rs}`: preserve protocol/cancellation; add only required backend-neutral/redacted task metadata and protocol parsing tests.
7. `src/gui/screens/storage/{mod.rs,pools.rs,providers.rs}`, then any callers in `src/gui/app.rs`, `src/gui/state/*`, and `src/gui/settings.rs`: wire UI last, retaining compatibility behavior and keeping I/O off-thread.

### Step 11 — exact files, in recommended order

1. Re-run static module/re-export/caller tracing, then remove only the five present unreachable files: `src/models/sync_config.rs`, `src/gui/task_runner.rs`, `src/gui/screens/maintenance/integrity.rs`, `src/gui/screens/maintenance/manifest.rs`, `src/gui/screens/maintenance/system.rs`.
2. `scripts/update-cleanup.nu`: preserve all legacy entries and append any newly retired exact paths caused by the pivot.
3. `README.md`, `DEVELOPMENT.md`, `CHANGELOG.md`, and the pivot architecture/migration/limitations/verification documents: reconcile boundaries and explicitly separate static review from unrun runtime gates.

**Cleanup-list rule:** the list must remain literal and explicit; before adding/removing a path, verify module/path/re-export/caller reachability. Keep the `Cargo.toml` existence plus `name = "rpool"` project guard, keep `--dry-run`, and never introduce broad globs, recursive directory deletion, or user-data scanning (`scripts/update-cleanup.nu:1-19`, `scripts/update-cleanup.nu:44-62`). An absent legacy path should generally remain listed because cleanup is for older installations, unless repository migration policy explicitly ends support for that upgrade source.

## 10-line summary

1. The active GUI executes operations as a background child of the current `rpool` executable, carrying the configured rclone path via `--rclone`.
2. `RPOOL_PROGRESS_PROTOCOL=1` and `@rpool-progress ` JSON on stderr are stable compatibility boundaries and must be preserved.
3. Stdout/stderr draining, structured-event filtering, cancellation/process-tree termination, and session-local retry are integral runner semantics.
4. Persisted task history is descriptive and redacted; it must never become an unvalidated retry/command source.
5. GUI storage/provider work must remain off the GUI thread; the existing child/background-thread model establishes that invariant.
6. Every one of the 22 explicit cleanup paths is dead or absent; none is active-via-path or ambiguous in this checkout.
7. Five listed dead files still exist and are valid Step 11 removal candidates after a final same-commit reachability check.
8. age, rclone obscure/config, and age-keygen calls are hardened secret/config wrappers—not `StorageBackend`; none of audited `config_sync` is archive storage I/O.
9. **Risk 1:** generic GUI command previews expose full argv; **Risk 2:** careless cleanup could delete folder-module successors; **Risk 3:** unconditional rclone diagnostics would break native-only configurations.
10. Verification here is static only: no build, test, binary, real tool, cloud, or secret operation was performed.
