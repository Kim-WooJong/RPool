# Crypt Secret Portability B1-B7 source-only verification

No Rust compiler, cargo, rclone, age, age-keygen, or Nushell execution was performed.
This audit is lexical/structural/pattern validation, not Rust type checking or an executed integration-test result.

- Rust source files: 248
- Reachable Rust modules: 247
- Static lexical/structural/pattern checks: 77 passed / 77
- Crypt Secret Portability Rust test functions written: 55; all unexecuted
- B6 ignored real-tool tests: 10
- Intermediate patch ZIP: not produced

## B6 acceptance coverage

- PASS (source harness): single crypt remote roundtrip
- PASS (source harness): multiple crypt remotes
- PASS (source harness): password2 present and absent
- PASS (source harness): exact obscured password/password2 byte equality
- PASS (source harness): existing encrypted sentinel cannot be read with placeholder keys and decrypts after restore
- PASS (source harness): repeated import is idempotent and leaves config bytes unchanged
- PASS (source harness): repeated export preserves the same logical obscured secret bundle and portable crypt definitions
- PASS (source harness): generated password/password2 values are independently generated and not reused across tested remotes
- PASS (source harness): wrong age identity failure before config mutation
- PASS (source harness): missing age identity failure before config mutation
- PASS (source harness): corrupted age vault failure before config mutation
- PASS (source harness): invalid schema and missing required password failure
- PASS (source harness): portable/secret remote mismatch failure
- PASS (source harness): target remote type != crypt failure
- PASS (source harness): test scenarios use temporary explicit --config files, not the user's ambient rclone.conf
- PASS (B5 deterministic tests): post-commit failure rollback state machine is covered with injectable mock failures

## B7 acceptance coverage

- PASS (source audit): top-level `rpool export` / `rpool import` CLI variants are connected
- PASS (source audit): CLI exposes no crypt password/password2 argument
- PASS (source audit): rclone config path can be explicit or discovered via `rclone config file`
- PASS (source audit): export extracts crypt secrets in memory and sends them directly to age
- PASS (source audit): artifact paths are fixed to `config/portable-config.json` and `secrets/rclone.age`
- PASS (source audit): portable JSON binds to the vault ciphertext with a BLAKE3 digest
- PASS (source audit): import validates artifact binding before portable settings are changed
- PASS (source audit): crypt preflight occurs before portable settings are changed
- PASS (source audit): age identity is required outside the artifact root and a public snapshot recipient can be derived with `age-keygen -y`
- PASS (source audit): crypt failure triggers rollback of portable rpool settings
- PASS (source audit): legacy JSON-only import refuses crypt-aware bundles
- PASS (source audit): user-facing command output contains paths/status only, not secret values

## Runtime status

The B6 tests are deliberately `#[ignore]`. They require real `rclone`, `age`, and `age-keygen` binaries and have not been executed here. Runtime verification on Windows/macOS/Linux remains pending.

Manual execution after the user confirms compilation:

```text
cargo test b6_real_ -- --ignored --test-threads=1
```

Do not treat this source audit as proof that the external tools executed successfully.

## Security invariants retained

- no crypt password deobscure/reveal API
- no config dump file/log persistence
- no plaintext secret temp file in export
- age private identity remains outside the portable artifact
- encrypted restore uses `--no-obscure` and never calls rclone obscure
- plaintext rclone.conf restore creates no additional plaintext staging/backup file
- encrypted snapshot and rollback guards remain active
- 1024-bit password and password2 generation remains independent per new crypt remote
- export/import never rotates existing crypt keys or fills a missing password2 on an existing remote
- mixed portable JSON / vault exports are rejected by ciphertext digest binding

B1-B7 source implementation is complete. The next planned development returns to v0.5.16 Settings / UI Persistence after user-side build validation.
