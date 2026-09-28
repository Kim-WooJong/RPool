# RPool Main Update 1 — Migration Map (Step 1)

작성일: 2026-09-23 (Asia/Seoul)
근거: MASTER_PROMPT §8 Step 2–12, 3개 expert lane audit.
검증 수준: 소스 기반. 각 Step은 실패를 숨기지 않고 checkpoint 남김.

---

## 1. 변경 순서 (dependency order)

### Step 2 — Shared type / 계약 / 오류

- **추가**: `src/storage/{mod,traits,capabilities,error,reference,registry}.rs`.
- **추가**: `src/models/storage.rs` / `src/models/volume.rs`, `src/models/mod.rs` 갱신.
- **금지**: manifest/root 파일에 ID 추가(ADR-001, §3.1).
- **금지**: default command 경로 교체(아직).
- **작성 test**: 주소 round-trip, Windows path 구별, legacy exact string 보존,
   range overflow/EOF, ID/Generation validation, unsupported operation, error classification.
- **완료 기준**: contract에 rclone/OpenDAL/GUI/Manifest implementation type이 새어 나오지 않음.

### Step 3 — MemoryBackend / deterministic failure injection

- 기존 trait를 실제 구현. 더 쉬운 별도 mock trait 만들지 않음.
- empty object/overwrite/delete/stat/bounded read/range.
- fault injection은 test-only wrapper. 특정 operation/call index의
   timeout/permission/not-found/corruption/short read/partial write/응답 유실/cancellation
   결정적 재현. sleep 대신 barrier/동기화 지점.
- **완료 기준**: rclone/OpenDAL/실제 계정 없이 transfer service에 주입 가능.
  mock이 모든 capability를 true로 반환하지 않음.

### Step 4 — LocalBackend

- 명시적 isolated root 내부만. traversal/absolute path/symlink/reparse escape/
  디렉터리 대상 충돌 처리. `canonicalize` 1회만으로 TOCTOU 해결 주장 안 함.
- streaming/range/limit. overwrite 시 임시 파일 수명/complete 후 publish/실패 시 기존 파일 보존.
- 동일 filesystem rename/atomic visibility/fsync durability 구별.
- cross-process CAS 검증 없이 제공 안 함. 미지원은 Unsupported.
- **완료 기준**: test는 temp root에만 접근. platform별 미검증 기능 분리 기록.

### Step 5 — RcloneBackend adapter

- **주요 대상**: `src/storage/{cat,stat,probe,copy,upload,download,quota,remote_config}.rs`.
- CLI 명령 구성 + subprocess 수명 관리를 rclone adapter 안으로.
- core-level hash/retry는 transfer 계층으로 분리.
- **crypt write gate 생략 안 함**(ADR-005).
- **R1 통합**: stat/probe error classification 통합.
- adapter 초기화에 명시적 executable/config context 전달.
- config cache를 executable string 하나만으로 식별하는 기존 가정 검토.
- secret operation의 sanitized process wrapper는 별도 보존.
- child stdin/stdout/stderr bounded 처리, early error/drop/cancel에서 kill/reap.
- rclone conditional write는 실제 증거 없으면 Unsupported.
- **완료 기준**: rclone I/O의 단일 owning implementation, 기존 free-function은 bridge로만.
  이중 경로 아님.

### Step 6 — Read / verify / restore 경로 DI

- **주요 대상**: `src/commands/{get,status,verify,scrub}.rs`,
  `src/manifest/{load,recover,replica_verify}.rs`, `src/maintenance/scan.rs`,
  `src/erasure/reconstruct.rs`, `src/journal/restore.rs`.
- command orchestration에서 registry/service 구성 + 하위 함수 주입.
- legacy manifest 해석은 compatibility resolver가 수행하되 **원본 주소 유지**.
- download + BLAKE3 verification을 backend-independent transfer로 연결.
- 기존 data/parity reconstruction + output offset 규칙 유지.
- NotFound/Corrupt/TransportError를 복구 정책에서 구별.
- status/scrub의 unknown을 healthy/missing으로 바꾸지 않음.
- **완료 기준**: 핵심 read 경로가 실제 trait 구현을 통해 흐름.
  기존 codec 재작성/새 manifest format 변경 없음.

### Step 7 — Write / resume / repair / migration 전환

- **주요 대상**: `src/commands/put.rs`, `src/storage/data_upload.rs`,
  `src/erasure/encode.rs`, `src/journal/upload.rs`, `src/manifest/replicate.rs`,
  `src/maintenance/repair.rs`, `src/provider/migrate.rs`.
- 실제 upload/parity/replica/repair/migration을 같은 injected service로 연결.
- **R2 제거**: size-only skip 제거, hash 기반 reusable-object 판단 공통화.
- source 변경/stale journal/원격 변경/retry 중단 의미 고정.
- journaling은 검증된 완료 상태만 기록.
- cross-backend copy: native server-side copy vs streaming copy 의미 구별.
- crypt별 key가 다르면 ciphertext를 raw backing 사이에서 그대로 복사 안 함.
- 기존 migration은 logical bytes 경로 + full verification 유지.
- 실패 시 manifest/replica/source deletion 순서 역전 안 함.
- `dry-run`은 mutation 안 함.
- **완료 기준**: 원격 쓰기의 정상 entry가 모두 암호화 policy + trait 경계 거침.

### Step 8 — Provider / GUI / diagnostic 경계

- **주요 대상**: `src/provider/health.rs`, `src/storage/{quota,remote_config}.rs`,
  `src/doctor/check.rs`, `src/application.rs`, `src/gui/usage_refresh.rs`,
  `src/gui/task/*`, `src/gui/screens/storage/*`, `src/remote_root/*`, `src/pool/*`.
- data I/O와 provider discovery/quota/tool diagnostics 분리.
- provider policy의 직접 rclone 명령을 적절한 adapter에 주입.
- physical capacity/alias/failure-domain 정보 구별.
- 확정 못 한 domain에 허위 장애 안전 보장 안 함.
- GUI의 기존 rclone config 관리는 compatibility UI로 남길 수 있음.
- 미구현 native credential editor/volume UI 만들지 않음.
- native-only/test backend 구성에서 `rclone version` probe 무조건 실행 안 함.
- backend-neutral work를 GUI thread로 옮기지 않음.
- progress protocol + cancellation 경로 보존.
- **완료 기준**: 남은 rclone-specific reference가 compatibility/admin/tool 경계로 설명 가능.

### Step 9 — Optional OpenDAL prototype

- Apache OpenDAL 공식 release/doc/source에서 정확한 version/feature/MSRV/runtime 요건 확인.
- 현재 Cargo.lock과 필요한 추가 dependency만 비교.
- optional dependency + 좁은 feature. 모든 service 활성화 안 함.
- 우선 Memory/Fs 기반 synthetic contract 경로.
- S3는 필요하면 별도 선택적 adapter configuration 경로까지 설계, 실계정 접속 안 함.
- OpenDAL type을 public StorageBackend contract 밖에 유지.
- native write security gate + default-off 상태 문서/source 연결.
- **완료 기준**: 실제 prototype source, doc-only stub 아님.
  local synthetic 경로에 rclone 불필요, default production 경로 변화 없음.
  lockfile 불일치 해결 불가하면 이 Step은 blocked.

### Step 10 — Metadata / crypt / legacy compatibility 통합 검토

- v1/v2 fixture + content-root expectation을 source baseline에서 보존.
- compatibility resolver의 round-trip이 legacy bytes를 변경하지 않는지 검토.
- 새 Object/Volume type이 기존 manifest JSON에 침투하지 않았는지 확인.
- B1–B7 코드/테스트의 불필요한 변경이 없는지 diff로 확인.
- native volume key는 미래 설계, 현재 crypt key와 혼합 안 함.
- MetadataStore/authority ADR에 미정 사항 + 다음 업데이트 blocking 조건.
- rollback은 source/config compatibility 관점, 원격 데이터 자동 rollback 약속 안 함.
- **완료 기준**: legacy 데이터를 재작성하지 않고 새 구조가 동작.
  미구현 distributed semantics를 완료로 표시 안 함.

### Step 11 — Source cleanup / 문서 / 검증 기록

- 더 이상 사용하지 않는 compatibility wrapper + 중복 implementation 정리.
- 각 제거 후보는 active module/re-export/caller 근거 확인.
- `scripts/update-cleanup.nu` explicit list 갱신. 광범위한 glob 삭제 금지.
- 기존 경고 기록 참고하되 현재 코드에 맞는 원인만 수정.
- blanket `allow(dead_code)`/`allow(unused)`로 경고 숨기지 않음.
- test-only/experimental 코드의 정당한 cfg gate 사용, 이유 기록.
- README/DEVELOPMENT/CHANGELOG/architecture/migration/limitations 일치.
- v0.5.16이 기존 Settings/UI Persistence에 예약됨 유지.
- version bump는 별도 결정 없이 안 함. 문서상 `next/unreleased: storage pivot`.
- **완료 기준**: `verification.md`에 확인/미실행 구분.
  warning 0/build 성공/cross-platform 통과 같은 미실행 주장 없음.

### Step 12 — Final source handoff / runtime gate

- 완료 matrix를 actual diff + test source로 하나씩 대조.
- 소스 구현 완료와 runtime/release 완료 분리.
- 변경/삭제 파일/dependency/미검증/blocked/다음 작업 보고.
- requested patch artifact가 있다면 변경 파일 + explicit cleanup 안내만.
- 기존 executable/target/credential/user data를 배포 ZIP에 포함 안 함.
- **완료 기준**: M1이 source-complete인지 blocked인지 증거로 설명.
  runtime gate가 남으면 release_ready=false.

## 2. security gate (모든 write 경로)

- shard/parity/repaired shard/migrated destination/manifest-replica의 모든 쓰기는
  **검증된 encryption binding을 거친다**.
- legacy rclone 경로: `ensure_crypt_destination` 정책 보존.
  destination은 configured `crypt` remote + `no_data_encryption != true`
  (`src/storage/rclone/mod.rs::RcloneContext::ensure_crypt`,
  `src/storage/writer.rs::StorageWriter::ensure_destination`).
- gate는 common authorized write path에 위치. backend primitive/retry/native copy/
  manifest helper/repair/migration이 우회 못 함.
- raw backing remote는 capacity administration에만, legacy archive write destination이 아님.
- 다른 crypt alias/key는 logical bytes를 전송 + 검증. ciphertext를 raw backing 사이에서
  복사 안 함(암호화 mapping identity가 증명되지 않는 한).

## 3. rclone 잔존 허용 경계 (ADR-006)

- legacy crypt archive 접근 + crypt portability는 rclone 의존 유지.
- doctor `rclone version`은 tool diagnostic, native-only 구성에서 gate.
- `config_sync/*`의 age/rclone/age-keygen은 secret wrapper, StorageBackend 아님.
- GUI `rpool` child는 orchestration, StorageBackend 아님.
- "subprocess 0개"를 목표로 GUI/secret 보안 wrapper를 해체 안 함.

## 4. Step 2에서 수정할 파일 (다음 보고)

- **추가**: `src/storage/{mod,traits,capabilities,error,reference,registry}.rs`
- **추가**: `src/models/storage.rs` / `src/models/volume.rs`
- **갱신**: `src/models/mod.rs` (새 module export)
- **금지**: `src/manifest/*`, `src/models/manifest.rs`, `src/models/coding.rs`,
   `src/models/portable_config.rs`, `src/config_sync/*` 변경(Step 10에서 검토만).
- **금지**: default command 경로 교체(Step 6/7에서).

## Step 11 checkpoint (2026-09-24)

Cleanup completed; see verification.md and step11-cleanup.json. Steps 0–12 are scoped source-complete; runtime/release gates remain pending (see final-handoff.md). Historical step requirements above remain the plan, not evidence of cross-platform execution.
