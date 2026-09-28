# RPool Main Update 1 — Pivot Baseline (Step 0)

작성일: 2026-09-23 (Asia/Seoul)
작업: Rust-native Storage Architecture Pivot
상태: Step 0 baseline 확정. Step 1 audit는 expert lane으로 진행 중.
검증 수준: **소스 기반(static)만**. cargo check/build/test/run/clippy/bench, 실제
rclone/age/age-keygen, 실제 cloud 접속은 **실행하지 않음**.

---

## 1. 실제 repository root와 변경 상태

- **repo root**: `~/src/rpool`
- **Git**: 없음 (`fatal: not a git repository`). 따라서 working-tree diff / untracked
  추적은 git으로 불가. baseline은 파일 단위 SHA-256으로 고정한다.
- **사용자 변경 보존**: reset/clean/강제 checkout/자동 commit·push **미실행**.
  untracked 파일 삭제 **미실행**.
- **이전 pivot checkpoint**: `docs/architecture/` 가 존재하지 않았음 → **Step 0이 처음**.
  재실행하지 않고 새로 작성.

## 2. 첨부본 대비 차이

- 첨부 ZIP: `rpool.zip`, 내부 최상위 `rpool/`.
- 현재 checkout의 Rust 소스 253개 파일을 첨부 `SOURCE_INVENTORY.json`과 1:1 대조:
   - inventory 파일 수: 253 / 현재 파일 수: 253
   - inventory에 있으나 현재에 없음: **0**
   - 현재에 있으나 inventory에 없음: **0**
   - **SHA-256 불일치: 0**
   - 총 라인: 현재 16067 / inventory 16067 (일치)
- **결론: 현재 checkout은 첨부 스냅샷과 byte-identical.** 차이는 없음.
   (첨부 `CRYPT_SECRET_PORTABILITY_STATE.json`의 `worktree: /mnt/data/rpool-work`와
    `source_archives` hash는 과거 Chat 작업 경로이며, 실제 checkout 경로가 아니다.
    이 문서의 기준은 실제 checkout이다.)

### 2.1 source fingerprint (이 baseline의 기준)

- per-file SHA-256 목록(정렬)의 SHA-256:
   `181585574705f2704229e9095694401ed0df5269ea9c5de67827f66f1361412a`
- Rust 파일 수: 253 / 총 라인: 16067
- per-file SHA-256 원본: `/tmp/current_rs_sha.txt` (이 환경의 임시 산출물;
  영구 기록은 `pivot-state.json`의 fingerprint 값으로 남김)

## 3. package / 구조

- `Cargo.toml`: package `rpool`, version `0.5.15`, edition `2021`,
   `autolib=false`, `autobins=false`, single `[[bin]] name="rpool" path="src/main.rs"`.
- **직접 의존성**: anyhow, eframe 0.36.2, rfd 0.17.2, blake3, clap 4(derive),
  rayon, reed-solomon-erasure 6, serde, serde_json, getrandom 0.3.4, tempfile 3.27.0.
- **OpenDAL / Tokio 직접 의존성: 없음** (E01 확인). → M1 기본 선택은
   **동기·object-safe·streaming StorageBackend 경계** (ADR-004).
- `main.rs`는 module 등록 + `application::run()` 진입점만. feature behavior 없음.
- `application.rs`는 CLI dispatch + cross-cutting history recording.
- `prelude.rs`는 공통 re-export.

## 4. active module vs cleanup candidate

- `main.rs`가 등록하는 top-level module: application, cli, commands, config,
  config_sync, doctor, erasure, gui, history, inventory, journal, maintenance,
  manifest, models, placement, planning, pool, provider, remote_root, prelude,
  presentation, progress, storage, utils.
- `scripts/update-cleanup.nu`의 `obsolete_files` 목록(21개)은 **명시적** cleanup 대상.
  파일명만으로 활성 여부를 판단하지 않음. 실제 `mod`/`#[path]`/re-export reachability는
  expert lane(gui.md)에서 R5로 확인 중. 대표 예:
   - `src/gui/task_runner.rs` vs `src/gui/task/runner.rs`
   - `src/gui/screens/maintenance/integrity.rs` vs `src/gui/screens/maintenance/integrity/mod.rs`
   - `src/gui/screens/maintenance/manifest.rs` vs `src/gui/screens/maintenance/metadata/manifest.rs`
   - `src/gui/screens/maintenance/system.rs` vs `src/gui/screens/maintenance/diagnostics.rs`
   - `src/models/sync_config.rs`
- **이 baseline 단계에서는 어떤 파일도 삭제하지 않음.** cleanup은 Step 11에서
  reachability 근거 확인 후 명시적 목록 갱신으로만.

## 5. B1–B7 / 검증 제한 / reserved scope

- `docs/CRYPT_SECRET_PORTABILITY_STATE.json` 기준:
   - B1–B5: source implemented (B4는 transactional entry만 노출).
   - B6: 10개 `#[ignore]` real-tool integration test **작성됨 / 미실행**.
   - B7: top-level `rpool export`/`import` orchestration implemented.
   - `release_complete: true`는 **source-only static validation** 결과이며,
    build/runtime 검증을 뜻하지 않음.
   - `cargo_check_build_test_run: false`, `rclone_age_nushell_executed: false`.
   - `rust_test_functions_written_not_run: 55`.
   - `existing_gui_warnings: 10` (출발 정보; 현재 warning 수로 단정하지 않음).
- **v0.5.16**은 Settings / UI Persistence(D1–D4)에 예약됨. 이번 pivot은
  v0.5.16을 소비하지 않음. version bump는 별도 결정 없이 하지 않음.

## 6. 이번 실행에서 하지 않을 것 (보호 범위)

- 제품 소스 대규모 변경, dependency 추가: **Step 0/1에서는 하지 않음**.
- cargo check/build/test/run/clippy/bench: **실행하지 않음**.
- 실제 rclone/age/age-keygen/cloud: **실행하지 않음**.
- 사용자 변경 덮어쓰기, credential/key/원격 데이터 변경: **하지 않음**.
- 같은 working tree를 여러 agent가 동시에 수정: **하지 않음**
   (expert는 read-only, 각자 전용 `_expert/<lane>.md`에만 기록).
- 소스 구현 상태와 실행 검증 상태를 분리: **미실행 검증을 통과로 보고하지 않음**.

## 7. 산출물

- `docs/architecture/pivot-baseline.md` (본 문서)
- `docs/architecture/pivot-state.json` (세션 간 상태)
- `docs/architecture/_expert/{transport,metadata,gui}.md` (expert lane audit, 진행 중)
- Step 1 산출물(종합 후 작성): `current.md`, `target.md`, `migration.md`,
   `compatibility.md`, `adr/ADR-001..006.md`

## 8. 다음

Step 1 — Architecture audit / ADR / migration map.
expert 3개 lane(transport / metadata / gui·secret·cleanup) 결과를 종합하여
KEEP/REFACTOR/REPLACE/REMOVE 분류, ADR-001~006 확정, R1–R5 현재 위치 확인,
Main Update 1과 이후 Data Layer 업데이트의 경계 명문화.
Step 1 종료 시 Step 2의 변경 파일과 완료 기준을 보고.
