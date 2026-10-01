# RPool Main Update 1 — Compatibility Contract (Step 1)

작성일: 2026-09-23 (Asia/Seoul)
근거: MASTER_PROMPT §3, §5.2, 3개 expert lane audit.
검증 수준: 소스 기반. legacy 데이터 재작성 금지.

---

## 1. 반드시 보존할 것 (별도 version migration 없이는 변경 금지)

### 1.1 manifest bytes

- pass-through/byte-identical recovery/replication이 약속된 경우 **원본 manifest bytes 유지**.
- `serde_json::to_vec[_pretty]`는 whitespace/key order/absent-vs-explicit default를
  보존하지 못한다(`src/manifest/fingerprint.rs:3-5`, `src/manifest/replicate.rs:19-24`,
  `src/manifest/recover.rs:35-38`). byte-identical 약속 시 원본 JSON bytes 유지.

### 1.2 manifest field/enum

- 모든 manifest field name/value/array order/enum spelling 보존.
- `coding: null`/absence semantics 보존(`src/models/manifest.rs:4-31`,
  `src/models/coding.rs:11-16`).
- 허용되는 default: top-level `coding: Option`(missing→None), shard `kind`(→Data),
  `group`/`slot`(→0). 그 외 필드는 필수.

### 1.3 address string

- exact `remote`/`object` UTF-8 string 보존(case/punctuation/path spelling 포함).
- v2 root에 `remote`/`object` UTF-8 byte length + bytes가 들어간다
  (`src/manifest/content_root.rs:47-50`).
- 새 `BackendId`/`ObjectKey`를 만들더라도 legacy `remote`/`object`를
  **자동 치환·정규화·재저장하지 않는다**.
- legacy 주소를 내부 reference로 해석하는 것과 manifest 포맷 migration은 **별개 작업**.

### 1.4 shard hash

- exact shard hash string bytes 보존(hash는 string으로 계산,
  `src/manifest/content_root.rs:9,51`).
- legacy `Shard.blake3`와 shard size는 **crypt가 외부로 노출하는 논리 shard bytes 기준**.
- raw backing의 ciphertext size/hash를 그 값과 직접 비교하거나
  ciphertext를 plaintext shard로 해석하지 않는다.

### 1.5 content-root domain

- **v1**: shard 순서대로 `index`(LE u32), `offset`(LE u64), `size`(LE u64),
  `blake3` string bytes(길이 prefix 없음). tag 없음.
- **v2**: `rpool-manifest-v2\0` tag + `original_size`/`shard_size`(LE u64) +
  coding byte + coding detail + per-shard `index`/kind byte/`group`/`slot`/`offset`/`size` +
   remote/object UTF-8 길이+bytes + `blake3` string bytes.
- `archive_id`/`original_name`/`created_unix`/root field는 두 root domain 밖.
- v1/v2 width/LE encoding/ordering/coding byte/v2 tag/v2 string length prefix 보존.

### 1.6 다른 persisted schema

- 기존 plan/journal/resume/inventory/pool/remote-root/snapshot/history/portable-config
   JSON/JSONL schema 보존.

### 1.7 crypt portability

- crypt fixed paths, JSON allowlist, vault binding, bundle schema, exact obscured secret bytes 보존.
- `rpool export/import` artifact 계약 보존(legacy JSON-only `rpool config export/import`는 제거됨; `rpool config paths`만 남음).
- `config/portable-config.json` + `secrets/rclone.age` artifact layout 보존.
- portable JSON에 crypt secret을 넣지 않음.
- obscured password/password2를 age 암호화 vault에 넣고 exact value로 복원.
- ciphertext BLAKE3 binding + import preflight 보존.
- age identity는 portable artifact root 밖.
- encrypted + plaintext rclone config 모두 transactional restore.
- export/import 중 추가 plaintext secret staging/backup 파일 만들지 않음.
- 기존 key 재생성 안 함. 기존 remote에 없던 password2를 import가 새로 만들지 않음.
- 신규 remote에만 password/password2 각각 1024-bit OS 난수(1024는 bit 수).
- provider credential/OAuth token은 portability feature 범위 밖.

### 1.8 GUI/progress

- GUI → `rpool` child + `RPOOL_PROGRESS_PROTOCOL=1` + `@rpool-progress ` JSON stderr protocol 보존.
- stdout/stderr 독립 drain, protocol-line suppression, cancellation/process-tree termination,
  session-local retry 보존.
- persisted history는 display/audit용, redacted. 실행 명령 소수가 될 수 없음.
- GUI 상위 navigation 6개 work area + 기존 theme 규칙 유지.
- provider migration의 Preview → Validation → Confirmation → Execution 유지.
- source 삭제는 opt-in, copy/full verification/manifest 저장/replica 갱신 뒤에만.
- repair는 transport error를 erasure로 간주 안 함.
- selective repair의 Full BLAKE3 pre-check + 후속 검증 유지.

## 2. 새 type의 침투 금지

- `BackendId`, `ObjectKey`, `Generation`, `VolumeId`, `FailureDomainId`,
  `CapacityDomainId`는 **v1/v2 JSON에 field로 침투하지 않는다**.
- 이들은 runtime/admin resolution fact.
- legacy address 해석은 compatibility resolver가 수행, **reversible/non-mutating**.
- format migration과 별개.

## 3. command 호환성

- 기존 command가 원래 지원하지 않던 operation을 호환성이라는 이름으로 억지 지원하지 않는다.
  예: provider drain은 v2를 요구하므로 v1 지원을 몰래 추가 안 함.
- CLI/GUI entry point는 기존과 호환되게 유지.
- `--rclone` compatibility argument 보존.
- 제거된 drive mode의 flag(v3/v5/v7, this-PC-only, full local replica)는 호환 대상이 아니다. 남은 command의 인자 호환은 보존.

## 4. version 관리

- 버전 규칙은 `docs/VERSIONING.md`를 따른다. persisted format 변경은 위 보존 계약을 깨지 않는 범위에서만 한다.

## 5. rollback

- rollback은 source/config compatibility 관점에서 제시.
- 원격 데이터 자동 rollback을 약속 안 함.
- legacy manifest는 M1에서 여전히 authority.
- MetadataStore/authority는 ADR-003 blocking 조건 충족 전까지 stub으로 등록 안 함.

## Step 10 검증 보완 (2026-09-24)

- 명시적 manifest replicate와 recover는 검증된 원본 JSON bytes를 유지한다.
  공백, key order, missing defaults를 다시 직렬화하지 않는다.
- 새 manifest 생성 및 provider migration 등 내용 변경 작업은 기존처럼 typed
  manifest를 직렬화한다. semantic fingerprint도 기존 규칙이며 raw-byte hash가 아니다.
- 복원은 같은 디렉터리의 임시파일을 persist하여 교체한다. fsync 기반 crash durability
  보장은 아니며 Unix 생성 권한은 tempfile의 제한된 기본 권한을 따른다.
- 고정 v1/v2 plain/RS fixture는 합성 데이터이며 기존 알고리즘과 문서의 LE preimage
  규칙을 독립적으로 구성해 얻은 root를 고정했다. 운영 사용자 데이터는 사용하지 않았다.
- MetadataStore/authority, native volume key, fencing, multi-writer, quorum/failover,
  reader/GC coordination은 구현 또는 검증 완료가 아니다. ADR-003 조건은 계속 차단 조건이다.
- rollback은 legacy source/config 호환성 범위이며 원격 데이터 자동 복구를 보장하지 않는다.
