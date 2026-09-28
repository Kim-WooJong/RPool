# RPool Main Update 1 — Current Architecture (Step 1)

작성일: 2026-09-23 (Asia/Seoul)
근거: 3개 expert lane audit(transport / metadata / gui) + main 직접 확인.
검증 수준: **소스 기반(static)만**. cargo/rclone/age/cloud 실행 없음.
expert 원본: `docs/architecture/_expert/{transport,metadata,gui}.md`

---

## 1. 진입점과 dispatch

- `main` → `application::run`. Clap `Cli`의 global `cli.rclone: String`이
  `dispatch`를 통해 command/GUI 진입점으로 **raw `&str`로 그대로 전달**된다
  (`src/application.rs:8-16,28-30`).
- backend object/registry가 없다. executable path가 raw string으로 끝까지 흐른다.
- no-command / `gui` 분기는 `gui::launch(&cli.rclone)`(`src/application.rs:11-13,30`).
- GUI task 실행은 **간접**: GUI가 `rpool` child를 시작하고
  `RPOOL_PROGRESS_PROTOCOL=1`을 설정, child가 CLI dispatch로 재진입한다
  (orchestration process boundary, storage backend가 아님).
- GUI usage refresh는 예외: worker thread에서 storage admin helper를 직접 호출
  (`src/gui/usage_refresh.rs:25-43`).

## 2. transport caller map (함수 단위)

### 2.1 쓰기 경로 (subprocess)

| 경로 | 호출 | subprocess |
|---|---|---|
| put data | `commands::put` → `upload_one_data_shard`(Rayon) → `remote_stat_size` + `upload_range` | `rclone rcat` |
| put parity | `commands::put` → `upload_parity_groups` → `erasure/encode.rs` → `remote_stat_size` + `upload_local_file` | `rclone rcat` |
| manifest replica | `replicate_manifest` → `ensure_crypt_destinations` → `rcat_bytes` | `rclone rcat` |
| repair | `scan_manifest` → `repair_manifest[_filtered]` → `repair_group` → full-hash + `upload_local_file` | `rclone rcat` |
| provider migration | `drain_manifest` → `copy_remote_object` + `verify_shard_full` → (선택) `delete_remote_object` | `rclone copyto` / `cat` / `deletefile` |

- 모든 remote write helper(`upload_range`, `upload_local_file`, `rcat_bytes`,
  `copy_remote_object`)는 subprocess 전에 `ensure_crypt_destination`을 호출한다
  (`src/storage/upload.rs:12,78,135`, `src/storage/copy.rs:5`).
- migration 순서: **copy → full verify → local manifest → replicas → opt-in deletion**
  (`src/provider/migrate.rs:106-135`).

### 2.2 읽기/검증/상태/복구 경로

| 경로 | 호출 | subprocess |
|---|---|---|
| manifest read | `load_manifest` → `read_remote_bytes` | `rclone cat`(whole-object `Vec<u8>`) |
| plain get | `get_plain` → `download_one_shard`(Rayon) | `rclone cat`(streaming) |
| erasure get | `get_erasure` → `reconstruct_group` → `download_shard_to_file` | `rclone cat` |
| verify | `verify_shard_quick`(`remote_stat_size`) / `verify_shard_full`(streaming BLAKE3) | `rclone lsjson` / `cat` |
| status | `commands::status` → `remote_stat_size`(Rayon) | `rclone lsjson` |
| scrub/repair | `scan_manifest` → `probe_shard` | `rclone lsjson` + optional `cat` |
| restore journal | `journal/restore.rs` | local state I/O(무 spawn) |

### 2.3 admin/planning 경로

- `build_upload_plan` → `assign_remotes` → free-ratio 시 `capacity_remotes_for` +
  `query_quota`(`rclone config dump` + `rclone about --json`).
- `collect_quota_reports` / `query_quota`: Rayon, `min(workers, remotes.len())` bound.
- `doctor/check.rs`: `list_rclone_remotes` + `rclone version` + crypt destination check.
- `provider/health.rs`: `check_providers` → `check_provider` → **직접 `rclone lsf`**
  (`src/provider/health.rs:8-20`) + capacity + `query_quota`.

## 3. R1–R5 현재 위치 (코드 근거)

### R1 — stat 오류 분류 불일치 (확인됨)

- `remote_stat_size`는 rclone spawn 성공 후 **모든 nonzero exit을 `Ok(None)`으로
  변환**한다. stderr/exit class를 보지 않는다(`src/storage/stat.rs:3-18`).
  → auth/permission/timeout/rate-limit/provider outage/malformed/true not-found가
  전부 "absent"로 합쳐진다.
- `probe_remote_size`는 stderr를 보고 6개 English missing substring만 `Ok(None)`,
  그 외 nonzero는 error(`src/storage/probe.rs:22-38`). `probe_shard`는
  `Ok(None)→Missing`, error→`Probe::Error`(`src/storage/probe.rs:3-13`).
- `remote_stat_size` caller:
  1. `upload_one_data_shard`(`src/storage/data_upload.rs:11`): false absence → 불필요한 overwrite 시도.
  2. `upload_parity_groups`(`src/erasure/encode.rs:46`): 동일.
  3. `verify_shard_quick`(`src/storage/verify.rs:5-9`): auth/rate-limit/timeout을 "missing shard"로 보고.
  4. `commands::status`(`src/commands/status.rs:22-30`): 전부 `missing` 증가, `errors` 아님.
- `probe_shard` caller: `scan_manifest`만(`src/maintenance/scan.rs:8-29`).
  `Probe::Error` count 보존, repair는 `Probe::Error` 존재 시 mutation 거부
  (`src/maintenance/repair.rs:52-69`). → 자동 repair 경로는 fail-closed guard 보유.
- **필요 수정**: 단일 typed stat이 `NotFound`를 별도 분류하고
  `Authentication/PermissionDenied/RateLimited/Timeout/Unknown`을 전파.
  status와 maintenance가 동일 분류를 소비.

### R2 — stale journal object re-adoption (확인됨)

1. `commands::put` → `validate_upload_journal` → completed shard마다 `verify_shard_full`
   (`src/commands/put.rs:98-109`, `src/journal/upload.rs:30-41`).
2. `verify_shard_full`이 rclone 실패/size mismatch/digest mismatch 시 `Err`
   (`src/storage/verify.rs:12-43`).
3. journal validator가 **모든 error를 `.is_err()`로 잡아 entry 제거**, reason 폐기
   (`src/journal/upload.rs:41-47`).
4. `commands::put`이 무효화한 journal 저장 + 해당 shard를 `data_plan`에 포함
   (`src/commands/put.rs:99-110`) → `upload_one_data_shard` 호출.
5. `upload_one_data_shard`가 즉시 `remote_stat_size`를 보고 **size가 같으면
   local source를 hash하고 `[skip]`**, remote byte 검증/쓰기 없이 manifest shard 생성
   (`src/storage/data_upload.rs:11-17`).
6. parity도 동일: size 같으면 skip, local expected hash 채택
   (`src/erasure/encode.rs:40-64`).
- **회귀 시나리오**: 같은 legacy address에 다른 bytes가 같은 length로 존재하면
  (same-length corruption, interrupted overwrite, reused plan/address) full-verify가
  mismatch를 잡아 entry를 제거하지만, size-only skip이 같은 corrupt bytes를
  "completed"로 재채택한다.
- **필요 수정**: reusable-object logic을 `transfer`로 이동, 재사용 전
  **logical-byte size + expected BLAKE3** 검증. 불명확한 transport error는 fail-closed.
  journal completion은 검증된 remote 완료 후에만 기록.

### R3 — transport 밖의 rclone 호출 (확인됨)

| 현재 호출 | 분류 | target boundary |
|---|---|---|
| `storage/*`의 `cat/lsjson/rcat/copyto/deletefile` | genuine storage I/O | `storage/backends/rclone` + `transfer` |
| `provider/health.rs`의 `rclone lsf` | backend admin | `storage/admin` (health policy는 provider domain) |
| `quota.rs` `about/listremotes`, `remote_config.rs` `config dump` | admin/config | `storage/admin` + crypt-policy context |
| `doctor/check.rs` `rclone version` | tool diagnostic | 별도 tool-diagnostic boundary(StorageBackend 아님) |
| `config_sync/*` age/rclone secret | secret wrapper | `config_sync` 유지(StorageBackend 아님) |
| `gui/task_runner.rs` rpool child | GUI process orchestration | GUI boundary 유지(StorageBackend 아님) |

- "모든 subprocess를 StorageBackend로 이동"은 **잘못된 목표**다.
  올바른 분할: data backend / backend admin / tool diagnostic / secret wrapper / GUI child.

### R4 — remote alias와 failure domain (확인됨)

- `warn_plan_failure_domains`는 `shard.remote.as_str()` 문자열별로 group을 세고
  가장 큰 name bucket을 parity와 비교(`src/planning/failure_domains.rs:3-26`).
- `manifest_single_provider_failure_safety`는 동일 알고리즘
  (`src/planning/failure_domains.rs:30-48`).
- consumer: `commands/put.rs:94-96`(warning), `commands/status.rs:136-140`(print),
  `provider/migrate.rs:60-67`(drain gate, `--allow-risky` 없으면 차단).
- **문제**: 별도 crypt 이름이 동일 계정/물리 storage를 가리킬 수 있다.
  다른 문자열 ≠ 독립 장애 도메인.
- **필요 수정**: `FailureDomainId` / `CapacityDomainId`를 `BackendId`/display name과
  별도 모델링. known-same / known-distinct / unknown으로 resolve.
  positive safety claim은 모든 관계가 known할 때만. unknown은 "Unknown/보장 불가".
  drain은 unsafe **또는 unknown**에서 fail-closed. quota는 known `CapacityDomainId`당
  1회 합산, unknown은 0/unlimited가 아님.

### R5 — 중복/폐기 파일 (확인됨)

- `scripts/update-cleanup.nu:19-42`의 22개 explicit path 전부 **DEAD 또는 absent**.
  ACTIVE-via-path / AMBIGUOUS 없음.
- **존재하는 dead 파일 5개**(Step 11 제거 후보):
  `src/models/sync_config.rs`, `src/gui/task_runner.rs`,
  `src/gui/screens/maintenance/integrity.rs`,
  `src/gui/screens/maintenance/manifest.rs`,
  `src/gui/screens/maintenance/system.rs`.
- **absent 17개**: legacy file-to-folder 잔재, older installation cleanup용 유지.
- cleanup script는 `Cargo.toml` 존재 + `name = "rpool"` guard, `--dry-run` 유지.

## 4. manifest v1/v2 contract (metadata lane)

- `load_manifest`는 version/layout/root을 검증하지 않고 deserialize만 한다
  (`src/manifest/load.rs:4-11`). recovery/replica_verify가 검증한다.
- 허용되는 default: top-level `coding: Option`(missing→None), shard `kind`(→Data),
  `group`/`slot`(→0). 그 외 필드는 필수.
- **v1 content root**: shard 순서대로 `index`(LE u32), `offset`(LE u64), `size`(LE u64),
  `blake3` string bytes(길이 prefix 없음). tag 없음(`src/manifest/content_root.rs:3-11`).
- **v2 content root**: `rpool-manifest-v2\0` tag + `original_size`/`shard_size`(LE u64) +
  coding byte + coding detail + per-shard `index`/kind byte/`group`/`slot`/`offset`/`size` +
  **remote UTF-8 길이(LE u64)+bytes, object UTF-8 길이+bytes** + `blake3` string bytes
  (`src/manifest/content_root.rs:14-53`).
- v2 address normalization은 root를 바꾼다. `archive_id`/`original_name`/`created_unix`/
  root field는 두 root domain 밖.
- `serde_json::to_vec[_pretty]`는 whitespace/key order/absent-vs-explicit default를
  보존하지 못한다 → byte-identical recovery 약속 시 원본 JSON bytes 유지 필요.

## 5. Crypt Secret Portability B1–B7 (metadata lane)

- B1–B5: source-complete(재구현). B4는 transactional entry만 노출.
- B6: 10개 `#[ignore]` real-rclone/age integration test **작성됨/미실행**.
- B7: top-level `rpool export/import` orchestration source-complete.
- `release_complete: true`는 **source-only static validation** 결과.
  build/runtime 검증을 뜻하지 않음.
- 77/77 static check는 lexical/structural/pattern(파서/타입 체크 아님).
- 55개 Rust test 함수 작성됨/미실행.
- invariant: portable JSON은 allowlist(crypt structure/binding, password/credential 아님),
  age vault는 obscured password/password2 exact restore, `secrets/rclone.age`는
  ciphertext BLAKE3 binding, age identity는 artifact root 밖, transactional restore,
  key 재생성/rotation 없음, 신규 remote에만 password/password2 각각 1024-bit OS 난수.

## 6. GUI/secret/cleanup (gui lane)

- GUI → current `rpool` child + `--rclone` + `RPOOL_PROGRESS_PROTOCOL=1` +
  `@rpool-progress ` JSON stderr protocol은 **compatibility invariant**.
- stdout/stderr 독립 drain, protocol-line suppression, cancellation/process-tree
  termination, session-local retry는 runner semantics.
- persisted history는 display/audit용, redacted. 실행 명령 소스가 될 수 없음.
- **위험 1**: generic GUI runner가 모든 argv를 visible command preview로 포맷
  (`src/gui/task/runner.rs:73-89,402-416`). secret-bearing portability/config op는
  argument classification/redaction 추가 전 이 runner 밖으로 유지.
- `config_sync/*`의 age/rclone/age-keygen은 **secret wrapper**, StorageBackend 아님.
- `doctor/check.rs` `rclone version`은 tool diagnostic, native-only 구성에서 gate 필요.

## 7. concurrency / retry / memory / deadline

- Rayon fan-out: put data/parity, get, verify, status, scan, migration, quota,
  provider health가 각각 local pool 생성. backend가 자체 pool을 추가하면 곱해진다.
  → **shared per-backend semaphore/budget** 필요.
- retry: upload/download/copy가 각각 `retries.max(1)`을 total attempts로 해석.
  adapter도 retry하면 곱해진다. → **retry owner 1개(transfer)** 선택.
- replay: upload가 매 attempt마다 local file을 reopen/reseek(재실행 가능하지만
  변경 감지 안 함). → replayable-source contract가 identity/size/expected hash 고정.
- streaming: shard upload/download/probe/full-verify가 `IO_BUFFER` loop,
  encode/reconstruct/repair가 stripe-sized block. 보존.
- whole-object: `read_remote_bytes`가 stdout 전체를 `Vec<u8>`(bounded metadata만 허용).
- deadline/cancel: subprocess에 explicit deadline 없음. streaming loop가 child pipe block.
  → `OperationContext` 전파 + `UnknownOutcome` write result.
- partial output: download-to-output이 최종 검증 전 쓰기. → partial sink mutation과
  verified completion 구분.
