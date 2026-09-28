# RPool Main Update 1 — Target Architecture (Step 1)

작성일: 2026-09-23 (Asia/Seoul)
근거: MASTER_PROMPT §4–§7, 3개 expert lane audit.
검증 수준: 소스 기반 설계. M1은 source-complete 목표, runtime 검증은 별도 gate.

---

## 1. 목표 모듈 배치 (방향, 전부 미리 생성 아님)

```text
src/
  storage/
    mod.rs            # module boundary: traits/registry/backends/admin
    traits.rs         # object-safe StorageBackend
    capabilities.rs   # BackendCapabilities
    error.rs          # StorageError taxonomy
    reference.rs      # ObjectRef / ObjectKey / BackendId
    registry.rs       # composition root: backend instance 생성/resolver
    backends/
      rclone/         # 기존 transport adapter
      local/          # isolated root
      memory/         # deterministic failure injection
      opendal/        # optional / experimental (feature-gated)
    admin/            # config/quota/probe 경계 (필요할 때)
  transfer/           # 검증/retry/ranged copy/shard 전송 공통 기능
  models/
    storage/          # 필요할 때만 domain type 하위 분리
    volume/           # ID/type 중심, legacy manifest 재작성 없음
  manifest/           # legacy metadata authority 유지
  erasure/            # 기존 codec 재사용
  provider/           # health/placement/migration policy
  config_sync/        # secret portability 소유권 유지
  progress/           # 기존 protocol 유지
```

## 2. 의존성 방향

```text
CLI / GUI orchestration
         ↓
Archive / Transfer / Maintenance services
         ↓
Backend registry + StorageBackend contract
         ↓
Rclone | Local | Memory | OpenDAL prototype
```

- metadata policy / credential management / provider administration은 관련 경계에서 병렬 분리.
- models가 storage 구현을 import하거나, backend가 GUI·manifest policy를 import하는
  **cycle을 만들지 않는다**.
- `transfer/` 추가는 동일 기능이 기존 storage helper에 이중 구현되지 않도록 한다.
- core callers는 injected registry/service를 받는다.
- 각 helper 내부에서 rclone adapter를 매번 생성하는 형식적 wrapper로 끝내지 않는다.
- `BackendKind`에 대한 거대한 match를 GUI/commands/erasure 곳곳에 복제하지 않는다.

## 3. StorageBackend 계약 (구현 요구사항, 복사할 완성 API 아님)

### 3.1 최소 type

`BackendId`, `ObjectKey`, `ObjectRef`, `ObjectMetadata`, `BackendCapabilities`,
`StorageError`, `ReadRange`, `WriteOptions`, `WriteReceipt`, `OperationContext`.
Volume 관련 `VolumeId`, `ObjectId`, `Generation`, `TransactionId`, `ShardId`는
별도 domain type. `ObjectId`와 storage `ObjectKey`는 같은 개념이 아님.
Generation은 overflow 검사, provider ETag/version과 혼동하지 않음.
새 ID 생성에 OS randomness/검증된 라이브러리 사용, timestamp만으로 유일성 보장 안 함.
**새 type 도입 때문에 기존 manifest에 mandatory field를 추가하지 않는다.**

### 3.2 주소와 key

- `ObjectRef = backend identity + key`. backend instance 생성은 composition root의
  registry/resolver가 맡는다.
- legacy rclone 주소는 **dedicated compatibility parser**가 처리.
- `remote:`와 Windows drive path를 무조건 첫 colon만으로 동일하게 파싱하지 않는다.
- 원본 legacy 문자열 보존, URL decode/case folding/Unicode normalization 몰래 적용 안 함.
- 새 native logical key에는 문서화된 separator/empty/parent/absolute-path 규칙.
- storage key와 실제 local filesystem path는 별도 변환.
- Local backend는 임의 cloud key를 그대로 디스크 경로로 붙이지 않는다.

### 3.3 필요한 operation

- stat: 존재·유형·size + 가능한 opaque version 정보.
- read / ranged read: caller가 지정한 sink 또는 명시적 streaming session.
- write: replay 가능한 source, 완료 상태 명확히 반환.
- delete: 단일 object, 없는 경우와 권한 오류 구별.
- list: bounded page/continuation. 지원하지 않으면 Unsupported.
- copy: 같은 backend에서 native copy만 명시적 노출.
- cross-backend copy는 별도 transfer service가 read → write → verify.
- move/rename은 선택적 capability. copy+delete를 atomic rename으로 표기 안 함.
- Backend trait에 Manifest/Coding/GUI/namespace transaction을 넣지 않는다.
- object safety 유지(비 generic method). `dyn StorageBackend` 가정 안 함.
- public contract는 OpenDAL type, rclone command string, secret-bearing config를 노출 안 함.

### 3.4 streaming/range/retry

- 파일/shard 전체를 무조건 `Vec<u8>`에 적재 안 함.
- small metadata convenience read에는 명시적 size limit.
- range는 half-open `[offset, offset+length)` + checked arithmetic.
- zero length/EOF/초과 range/short read/추가 bytes 동작을 test로 고정.
- 여러 range가 같은 object version을 읽도록 pinning 또는 immutable-object 전제.
- 지원 없는 range를 전체 download로 몰래 대체 안 함.
- retry는 source를 처음 상태로 다시 열 수 있을 때만.
- replay 중 source file 변경 감지/제한.
- adapter와 상위 layer의 중복 retry로 최대 시도 수 곱해지지 않게.
- read sink의 partial bytes 상태와 verified 완료 상태 구분.
- write 응답 유실은 "아무것도 안 써졌다"가 아님. uncertain outcome 표현.
- cancellation은 이미 성공한 원격 write의 자동 rollback이 아님.

### 3.5 error taxonomy

`NotFound`, `AlreadyExists`, `PermissionDenied`, `Authentication`, `RateLimited`,
`Timeout`, `Cancelled`, `InvalidInput`, `Unsupported`, `PreconditionFailed`,
`CorruptData`, `TransientIo`, `UnknownOutcome`, `Other`.
- 모든 오류를 `Option::None`/empty list/Missing으로 줄이지 않는다.
- rate-limit retry-after 등 유용한 정보는 보존, secret/raw command는 보존 안 함.
- 불명확한 rclone 종료 상태를 NotFound로 추정 안 함.
- 삭제/repair/resume 결정은 typed error 이용.

### 3.6 capability

- 모든 backend가 같은 의미를 제공한다고 가정 안 함.
- read/range/stream/write/list/copy/rename, conditional create/update/delete,
  version pinning, size/part 제한, consistency scope 표현.
- `unknown`은 지원으로 처리 안 함.
- ETag 존재만으로 CAS 지원 true로 안 함.
- provider 조건부 operation과 RPool fallback 구별.
- atomic replace와 power-loss durability 구별.
- rclone adapter의 조건부 쓰기는 실제 검증 전까지 Unsupported.
- Local CAS는 cross-process 보장 없이 process-local mutex면 그 범위 표시.
- Memory CAS가 동작한다고 cloud CAS를 광고 안 함.
- provider/서비스 이름으로 capability 하드코딩 안 함.
- exact release에 없는 OpenDAL field/API 추측해 사용 안 함.

## 4. R1–R5 target

- **R1**: 단일 typed stat. `NotFound` 별도 분류, `Authentication/PermissionDenied/
  RateLimited/Timeout/Unknown` 전파. status와 maintenance가 동일 분류 소비.
- **R2**: reusable-object logic을 `transfer`로 이동. 재사용 전
  **logical-byte size + expected BLAKE3** 검증. 불명확한 transport error는 fail-closed.
  journal completion은 검증된 remote 완료 후에만 기록.
- **R3**: data backend / backend admin / tool diagnostic / secret wrapper / GUI child
  5분할. `provider/health.rs`의 `rclone lsf`는 `storage/admin`으로, health policy는
  provider domain. `doctor/check.rs` `rclone version`은 tool diagnostic.
- **R4**: `FailureDomainId`/`CapacityDomainId`를 `BackendId`/display name과 별도.
  known-same/known-distinct/unknown resolve. positive safety claim은 모든 관계 known할 때만.
  drain은 unsafe 또는 unknown에서 fail-closed. quota는 known `CapacityDomainId`당 1회.
- **R5**: 5개 존재 dead 파일 제거(Step 11), 17개 absent는 cleanup list 유지.

## 5. ADR 요약

- **ADR-001**: archive engine ≠ filesystem. M1은 transport coupling 제거.
- **ADR-002**: StorageBackend(bytes) vs MetadataStore(generation/namespace/commit/lease).
  MetadataStore는 M1에서 문서화만, stub 아님.
- **ADR-003**: consistency authority 8가지 명시. M1은 문서화만, 13개 blocking 조건.
- **ADR-004**: 동기·object-safe·streaming 경계 보존. async 변환 안 함.
- **ADR-005**: native crypto 미구현. OpenDAL은 test-only/experimental.
  정상 write는 encryption binding 요구.
- **ADR-006**: rclone optional ≠ 제거. legacy crypt archive/portability는 rclone 유지.

## 6. M1과 M2 경계

- **M1 (Main Update 1)**: storage abstraction 전환.
   legacy archive engine의 transport coupling 제거, backend-neutral 계약,
   R1–R5 수정, crypt portability 보존, GUI progress 보존.
- **M2 (Main Update 2)**: Native Data Layer.
   namespace, mount, distributed transaction, native encryption/format,
   MetadataStore/authority, multi-client write.
- M1에서 M2 type을 도입해도 namespace/transaction이 생기는 것은 아님(ADR-001).
