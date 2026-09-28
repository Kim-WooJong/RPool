# Metadata, Recovery, Compatibility, and Crypt Portability Audit

**Scope:** Main Update 1, Step 0/1, metadata lane.  
**Evidence status:** source inspection only. No Cargo command, Rust test, binary, real `rclone`, real `age`, or cloud operation was run. “Source-complete” below means only that the checked-in source/docs claim and structurally contain the implementation; it is not runtime verification.

## 1. Manifest v1/v2 contract

### Reader and wire schema

`load_manifest` reads either exact local file bytes or remote bytes and directly deserializes `Manifest`; loading alone does **not** validate version, layout, or root (`src/manifest/load.rs:4-11`). Recovery and replica verification deserialize and validate (`src/manifest/recover.rs:21-32`, `src/manifest/replica_verify.rs:32-39`).

The persisted schema is `Manifest`/`Shard` (`src/models/manifest.rs:4-31`). Only these fields tolerate absence:

- top-level `coding`: `#[serde(default)] Option<Coding>`, so missing becomes `None` (`src/models/manifest.rs:13-15`);
- shard `kind`: defaults to `Data` (`src/models/manifest.rs:26-27`, `src/models/coding.rs:18-21`);
- shard `group` and `slot`: default to zero (`src/models/manifest.rs:28-31`).

All other fields are required. `coding: null` is equivalent to no coding. When present, all four `Coding` fields are required (`src/models/coding.rs:3-9`).

### Validation and authoritative fields

`version` selects validation/hash semantics; only 1 and 2 are accepted (`src/manifest/validate.rs:5-10`).

For **v1**, ordered shard `index` must equal vector position, offsets must be contiguous with checked size addition, their sum must equal `original_size`, and `content_root_blake3` must match v1 (`src/manifest/validate.rs:13-36`). The validator does not consult `coding`, `shard_size`, `kind/group/slot`, `remote`, or `object`. Nevertheless `remote/object` are operationally authoritative addresses and must not be normalized merely because v1 omits them from the root.

For **v2**, all physical indexes are contiguous (`src/manifest/validate.rs:39-44`). Data shards are selected by `kind == Data`; their indexes/offsets/sizes and final `original_size` are checked (`src/manifest/query.rs:3-9`, `src/manifest/validate.rs:46-65`). With coding, algorithm/counts/nonzero stripe, parity count, and every data/parity index-kind-group-slot-size relation are validated (`src/manifest/validate.rs:67-112`). Without coding, parity is invalid (`src/manifest/validate.rs:113-119`). Finally all v2 hash-domain values are authoritative through the root check (`src/manifest/validate.rs:121-129`).

### Exact frozen content-root domains

**v1** feeds BLAKE3, for each shard in stored order: `index` LE u32, `offset` LE u64, `size` LE u64, then the UTF-8 bytes of the `blake3` string with no length prefix. There is no tag and no other field (`src/manifest/content_root.rs:3-11`).

**v2** feeds BLAKE3 in order:

1. literal `rpool-manifest-v2\0`;
2. `original_size`, `shard_size` as LE u64;
3. coding byte 1/0;
4. for `Some`: algorithm UTF-8 byte length as LE u64, algorithm bytes, then data/parity/stripe counts cast to LE u64;
5. per stored shard: `index` LE u32; kind byte Data=0/Parity=1; `group` LE u32; `slot` LE u16; `offset` and `size` LE u64; **remote UTF-8 byte length as LE u64 + bytes; object UTF-8 byte length as LE u64 + bytes**; then `blake3` string bytes without a length prefix (`src/manifest/content_root.rs:14-53`).

Thus v2 address normalization changes the root. `archive_id`, `original_name`, `created_unix`, and the root field itself are outside both root domains.

### Required round trip

Semantic round-trip must preserve every field/value and exact `remote/object` strings; missing default fields must remain readable and must not be silently persisted as an implicit migration. Where pass-through or byte-identical recovery is promised, retain original JSON bytes. Current `serde_json::to_vec[_pretty]` cannot preserve whitespace, key order, or absent-vs-explicit-default spelling (`src/manifest/fingerprint.rs:3-5`, `src/manifest/replicate.rs:19-24`, `src/manifest/recover.rs:35-38`).

`BackendId`, `ObjectKey`, `Generation`, and `VolumeId` must not become fields, wrappers, or changed encodings in `Manifest`/`Shard` JSON. A compatibility resolver may map exact legacy `(remote, object)` strings to runtime types, but interpretation is separate from format migration (Master Spec `:632-645,740-758`).

## 2. R4 confirmation: aliases invalidate string-counted safety

`warn_plan_failure_domains` groups by coding group and `shard.remote.as_str()`, then takes the largest name bucket (`src/planning/failure_domains.rs:3-16`). It emits a positive “safe” claim when that count is within parity (`src/planning/failure_domains.rs:17-26`). `manifest_single_provider_failure_safety` repeats the same algorithm (`src/planning/failure_domains.rs:30-48`).

| Consumer | Use |
|---|---|
| `src/commands/put.rs:94-96` | Placement warning/positive claim. |
| `src/commands/status.rs:136-140` | Prints single-provider safety. |
| `src/provider/migrate.rs:60-67` | Gates provider drain unless `--allow-risky`. |

Migration rewrites preview addresses, recomputes v2 root, validates, then invokes this check (`src/provider/migrate.rs:45-61`), so R4 is safety-critical.

Distinct crypt names may resolve to one backing account, bucket, drive, quota pool, or physical storage. Different strings therefore do not prove independent loss domains. Required design:

- model `FailureDomainId` and `CapacityDomainId` separately from `BackendId` and display/config names;
- resolve from explicit, auditable backend/admin evidence as known-same, known-distinct, or unknown;
- allow a positive safety claim only when all needed relations are known; unknown means **Unknown/not guaranteed**, never independent-by-default;
- drain must fail closed (or require the explicit risky override) for unsafe **or unknown**;
- aggregate quota once per known `CapacityDomainId`; unknown remains Unknown/error, never 0 or unlimited. Current quota values are optional but domain provenance is absent (`src/models/quota.rs:3-12`).

This confirms Master Spec R4 (`:413-422`).

## 3. Crypt Secret Portability B1-B7

### Invariants

1. Portable JSON is an allowlist with crypt structure/binding, never password/password2 or provider credentials (`src/models/portable_config.rs:21-44,68-94`); provider/OAuth credentials are out of scope (Master Spec `:190`).
2. The age vault holds already-obscured `password` and optional `password2`; exact stored bytes restore. No reveal API (`src/models/secrets.rs:11-18`; `docs/CRYPT_SECRET_PORTABILITY.md:36-38`).
3. Fixed `secrets/rclone.age` is bound by ciphertext BLAKE3; mixed/stale pairs fail before mutation (`src/models/portable_config.rs:4-7,46-64`; `src/config_sync/artifact.rs:79-115`). Import preflights package, binding, identity, target, schema, names, type, and structure (`docs/CRYPT_SECRET_PORTABILITY.md:147-152`).
4. Private age identity stays outside artifact root (`docs/CRYPT_SECRET_PORTABILITY.md:30-32`; `docs/CRYPT_SECRET_PORTABILITY_STATIC.md:42-43,64`).
5. Restore remains transactional for encrypted and plaintext config. Encrypted uses ciphertext staging/replacement; plaintext patches in memory/directly in the existing inode with no extra plaintext staging/backup (`docs/CRYPT_SECRET_PORTABILITY.md:46-83`). Its weaker crash atomicity remains explicit (`:95-101`).
6. Export/import/restore never regenerates, rotates, fills, or repairs keys; absent password2 clears stale salt rather than inventing one (`docs/CRYPT_SECRET_PORTABILITY.md:36-38,153-157`).
7. Generation is new-remote-only, rejects an existing name, and independently draws 1024 bits of OS randomness for password **and** password2 (`docs/CRYPT_SECRET_PORTABILITY.md:33-35`).
8. Secret process data remains bounded/in-memory/sanitized and unlogged; buffer clearing is best-effort only (`docs/CRYPT_SECRET_PORTABILITY.md:21-29`).
9. `config_sync` retains ownership; it is not generic `StorageBackend` I/O (Master Spec `:403-411`).

### Stage status

| Stage | Source-observed status | Runtime status |
|---|---|---|
| B1 | Reimplemented/source-complete checkpoint. | Unverified; old implementation equality not established (`docs/CRYPT_SECRET_PORTABILITY_STATE.json:16-20`). |
| B2 | Reimplemented/source-complete checkpoint. | Unverified; same provenance limitation (`:16-20`). |
| B3 | Reimplemented/source-complete checkpoint. | Unverified; same provenance limitation (`:16-20`). |
| B4 | Reimplemented; only transactional entry exposed. | Unverified (`:21`). |
| B5 | Source-complete for encrypted/plaintext; no extra plaintext file. | Unverified (`:22`; design `docs/CRYPT_SECRET_PORTABILITY.md:46-101`). |
| B6 | Ten ignored real-rclone/age integration tests written. | **Not executed** (`docs/CRYPT_SECRET_PORTABILITY_STATE.json:23,37-38`; `docs/CRYPT_SECRET_PORTABILITY_STATIC.md:1-10,47-57`). |
| B7 | Source-complete package orchestration, binding, preflight, rollback coupling. | Unverified (`docs/CRYPT_SECRET_PORTABILITY_STATE.json:24`; `docs/CRYPT_SECRET_PORTABILITY_STATIC.md:32-45`). |

The checkpoint explicitly says B1-B7 source complete/B6 runtime pending (`docs/CRYPT_SECRET_PORTABILITY_STATE.json:3`), 55 Rust tests written-not-run, no Cargo execution, and no rclone/age/Nushell execution (`:36-38`). Its 77/77 checks are lexical/structural/pattern only, not parsing/type checking (`:31-35`).

## 4. KEEP / REFACTOR / REPLACE / REMOVE

| File | Current role | Decision | Reason | Target module |
|---|---|---|---|---|
| `src/manifest/content_root.rs` | v1/v2 roots | KEEP | Frozen hash domains. | `manifest` |
| `src/manifest/fingerprint.rs` | Logical serialized fingerprint | KEEP | Replica logical equality; not input-byte identity. | `manifest` |
| `src/manifest/load.rs` | Local/rclone reader | REFACTOR | Inject backend/resolver; preserve bytes/strings. | `manifest` + transfer |
| `src/manifest/query.rs` | Pure shard helpers | KEEP | Format-domain logic. | `manifest` |
| `src/manifest/recover.rs` | Replica scan/recovery | REFACTOR | Inject reads; retain validation/ID and raw bytes if promised. | recovery service |
| `src/manifest/replica_verify.rs` | Replica validation | REFACTOR | Backend-neutral reads and typed uncertainty. | `manifest` + transfer |
| `src/manifest/replicate.rs` | Replica writes | REFACTOR | Inject writes; preserve crypt gate/schema. | `manifest` + transfer |
| `src/manifest/targets.rs` | Unique remote names | KEEP | Legacy view, not domain resolution. | compatibility view |
| `src/manifest/validate.rs` | Versioned validation | KEEP | Compatibility authority. | `manifest` |
| `src/manifest/mod.rs` | Facade | REFACTOR | Export injected APIs/resolver. | `manifest` |
| `src/models/manifest.rs` | Persisted schema | KEEP | Frozen JSON; no new IDs. | models |
| `src/models/coding.rs` | Coding schema | KEEP | v2 root/JSON contract. | models |
| `src/models/placement.rs` | Placement wire enum | KEEP | Persisted/CLI contract. | models |
| `src/models/pool.rs` | Pool config | REFACTOR | Keep wire form; resolve IDs externally. | pool/resolver |
| `src/models/portable_config.rs` | Portable allowlist/binding | KEEP | B7 security contract. | config_sync |
| `src/models/secrets.rs` | Vault schema | KEEP | Security contract. | config_sync |
| `src/models/sensitive.rs` | Sensitive buffers | KEEP | Security boundary. | config_sync |
| `src/models/sync_config.rs` | Retired duplicate bundle | REMOVE | Not in `models/mod.rs`; docs mark retired (`docs/CRYPT_SECRET_PORTABILITY.md:195-202`). | none |
| `src/models/upload.rs` | Plan/runtime upload models | REFACTOR | Preserve plan JSON; runtime refs stay external. | planning/storage |
| `src/models/resume.rs` | Resume state | KEEP | Existing persisted state. | journal |
| `src/models/journal.rs` | Upload completion state | REFACTOR | Preserve reader; runtime refs via resolver. | journal |
| `src/models/integrity_snapshot.rs` | Health snapshot | REFACTOR | Unknown must not collapse to healthy/missing. | maintenance |
| `src/models/inventory.rs` | Derived index | REFACTOR | Version derived additions; retain source strings. | inventory |
| `src/models/history.rs` | JSONL record | KEEP | Stable/redacted audit record. | history |
| `src/models/provider.rs` | Health DTO | REPLACE | Boolean/name cannot express domains/Unknown. | provider domain |
| `src/models/quota.rs` | Quota DTO | REPLACE | Needs capacity-domain/provenance/Unknown. | admin/capacity |
| `src/models/probe.rs` | Read result | REFACTOR | Preserve missing/corrupt/error; use typed errors. | transfer |
| `src/models/remote_root.rs` | Legacy roots | REFACTOR | Keep strings; resolve externally. | compatibility |
| `src/models/put_options.rs` | Runtime put options | REFACTOR | Carry resolved runtime refs. | planning DTO |
| `src/models/mod.rs` | Model facade | REFACTOR | New runtime types in separate owner. | models::storage/volume |
| `src/planning/failure_domains.rs` | String safety count | REPLACE | R4 makes positive guarantee unsound. | planning/domain resolver |
| `src/planning/upload_plan.rs` | Legacy v2 plan builder | REFACTOR | Inject backend selection; preserve output strings. | planning |
| `src/planning/shard_conversion.rs` | Plan-to-manifest copy | KEEP | Prevent runtime IDs leaking into manifest. | planning adapter |
| `src/inventory/add.rs` | Validate/index manifest | REFACTOR | Inject loader; keep validation first. | inventory |
| `src/inventory/entry.rs` | Manifest projection | KEEP | Pure derived metadata. | inventory |
| `src/inventory/load.rs` | Local index load | KEEP | Cache, not commit authority. | inventory |
| `src/inventory/query.rs` | Search | KEEP | Pure query. | inventory |
| `src/inventory/rebuild.rs` | Local scan/rebuild | KEEP | Valid local cache rebuild. | inventory |
| `src/inventory/save.rs` | Atomic local save | KEEP | Correct for cache, not distributed CAS. | inventory |
| `src/inventory/mod.rs` | Facade | KEEP | Coherent cache boundary. | inventory |
| `src/history/append.rs` | JSONL append | KEEP | Preserve format. | history |
| `src/history/load.rs` | JSONL load | KEEP | Compatibility reader. | history |
| `src/history/prune.rs` | Retention rewrite | KEEP | Local maintenance. | history |
| `src/history/redact.rs` | Redaction | KEEP | Security baseline. | history |
| `src/history/track.rs` | Command-to-record | REFACTOR | New commands without credentials/authority. | history/app |
| `src/history/mod.rs` | Facade | KEEP | Coherent ownership. | history |
| `src/config_sync/mod.rs` | B1-B7 boundary | KEEP | Not generic storage. | config_sync |
| `src/config_sync/artifact.rs` | Paths/digest binding | KEEP | B7 invariant. | config_sync |

## 5. Compatibility contract

Must survive unchanged unless separately version-migrated:

- original manifest bytes where pass-through/byte-identical recovery/replication is promised;
- all manifest field names/values/array order/enum spelling and coding null/absence semantics (`src/models/manifest.rs:4-31`, `src/models/coding.rs:11-16`);
- exact remote/object UTF-8 strings, including case/punctuation/path spelling (in v2 root at `src/manifest/content_root.rs:47-50`);
- exact shard hash string bytes (hashed as strings at `src/manifest/content_root.rs:9,51`);
- v1/v2 widths, LE encodings, ordering, coding byte, v2 tag, and v2 string length prefixes;
- existing plan, journal, resume, inventory, pool, remote-root, snapshot, history, and portable-config JSON/JSONL schemas;
- crypt fixed paths, JSON allowlist, vault binding, bundle schema, and exact obscured secret bytes.

`BackendId`, `ObjectKey`, `Generation`, `VolumeId`, `FailureDomainId`, and `CapacityDomainId` stay out of v1/v2 JSON. They are runtime/admin resolution facts. Legacy address interpretation by a compatibility resolver is separate from format migration and must be reversible/non-mutating.

## 6. Step touch order and MetadataStore blockers

1. **Step 2:** add `src/storage/{mod,traits,capabilities,error,reference,registry}.rs`; add `src/models/storage.rs` and/or `volume.rs`, update `src/models/mod.rs`. Do not add IDs to manifest/root files (Master Spec `:533-548`).
2. **Step 6:** `src/manifest/{load,recover,replica_verify,mod}.rs`, then `src/commands/{get,status,verify,scrub}.rs`, `src/maintenance/scan.rs`, `src/erasure/reconstruct.rs`, `src/journal/restore.rs`; add compatibility resolution while retaining original bytes/strings (Master Spec `:624-645`).
3. **Step 7:** `src/commands/put.rs`, `src/storage/data_upload.rs`, `src/erasure/encode.rs`, `src/journal/upload.rs`, `src/manifest/replicate.rs`, `src/maintenance/repair.rs`, `src/provider/migrate.rs`; touch planning conversion only for runtime-to-legacy bridging (Master Spec `:649-675`).
4. **R4 before positive claims:** replace `src/planning/failure_domains.rs`; update `src/commands/{put,status}.rs`, `src/provider/migrate.rs`, `src/models/{provider,quota}.rs`, and provider/admin resolution (Master Spec `:679-704`).
5. **Step 10:** inspect/prefer no changes to `src/models/{manifest,coding,portable_config}.rs`, `src/manifest/{content_root,validate}.rs`, fixtures, and `src/config_sync/*`; add exact-root/round-trip/default tests and retain 10 ignored B6 tests (Master Spec `:740-758`).

Future MetadataStore/authority is blocked until ADR-002/003 decides and validates: one writer-shared commit authority; root pointer and immutable layout; real conditional CAS (not read/compare/overwrite); fencing plus lease enforcement; commit visibility/durability and lost-response idempotency; orphan/pending lifecycle; reader/GC race protocol; stale/offline conflict policy; replica backup-vs-consensus role and recovery; enforceable single-writer or refusal; separately designed failover/quorum/offline multi-master; and capability/unknown-outcome behavior. These are required by Master Spec `:215-241`. M1 documents only: no success stub and no claim that Local/Memory CAS proves cloud safety (`:221-239`). The legacy manifest remains authority in M1 (`:455-462`).

## 10-line summary

1. Only v1/v2 are valid; missing coding and shard kind/group/slot are the manifest compatibility defaults.
2. v1 hashes ordered index/offset/size/hash-string bytes and excludes addresses.
3. v2 is frozen and hashes coding/layout plus length-prefixed remote and object UTF-8 bytes.
4. Preserve legacy address strings and original JSON bytes where byte identity is promised; Serde is not byte-preserving.
5. New backend, object, generation, volume, failure-domain, and capacity-domain types must not leak into v1/v2 JSON.
6. R4 is confirmed: remote-name counting can falsely claim single-provider-loss safety across aliases.
7. R4 affects put, status, and provider-drain gating; unknown aliases must be Unknown/not-guaranteed.
8. B1-B7 are source-complete only; B6 has 10 ignored real-tool tests and none were executed.
9. Top risks: alias safety/quota error; accidental v2 address/root rewrite; weakening transactional secret invariants.
10. MetadataStore remains blocked on real authority/CAS/fencing semantics; M1 must document, not stub, it.
