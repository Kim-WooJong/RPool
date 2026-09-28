# Crypt Secret Portability — B1-B7 completed source checkpoint

Date: 2026-09-23. Internal work only; no patch ZIP/release was produced.

## Source provenance

The accessible source inputs for this checkpoint were `rpool.zip` and
`rpool-v0.5.15-baseline-build-hotfix.zip`. Previously described B1–B4 working
files were not present in this runtime or found in the available file search.
Their required foundation was reimplemented here; equality with the previously
reported implementation is NOT established. Do not claim the old 36-check or
12-test reports apply to this working tree.

`Cargo.toml` remains v0.5.15. Its stale v0.5.12 root package entry in Cargo.lock
was aligned to v0.5.15. getrandom 0.3.4 and tempfile 3.27.0 were already locked
transitive packages; they are now direct dependencies. No dependency version
upgrade, build, cargo command, or external age/rclone execution was performed.

## Implemented ownership

- models/portable_config.rs: credential-free portable crypt structure allowlist.
- models/secrets.rs: schema v1, obscured fields, optional password2, multiple
  remotes, duplicate-key/unknown-field rejection. No Debug/Display for secrets.
- models/sensitive.rs: guarded buffers with best-effort clearing, NOT guaranteed
  erasure of allocator/compiler/subprocess/swap/crash-dump copies.
- config_sync/secret_process.rs: bounded in-memory child output, concurrent pipe
  draining, direct-child timeout/reaping, closed stderr, sanitized rclone env.
- config_sync/crypt_secrets.rs: rclone config dump read in memory, narrow typed
  parser, no generic all-credential JSON model or dump file.
- config_sync/age_vault.rs: streaming serialization into age stdin, same-volume
  ciphertext temp, file sync, close-before-persist, age decrypt success checked
  before payload parsing. Private identity must be outside artifact_root.
- config_sync/crypt_generate.rs: independent 1024-bit OS randomness for each
  new crypt remote's password/password2; existing remote names rejected.
  The representation is 171 unpadded Base64URL characters, not 1024 characters.
- config_sync/crypt_restore.rs: portable/schema/name/type/structure validation,
  explicit --no-obscure, no generation on restore, absent password2 clears any
  stale target salt, exact stored-value comparison, B5-only mutation gateway.
- config_sync/plaintext_config.rs: strict in-memory editor for only crypt
  password/password2 in an existing plaintext rclone.conf; duplicate section/key
  rejection and best-effort clearing of temporary line buffers.
- config_sync/transaction/: format-aware file lock, age snapshot, encrypted-stage
  or plaintext in-memory candidate, commit, rollback, interrupted-transaction
  recovery, and source-only regression tests.

## B5 transaction

1. Validate portable config; decrypt the whole age vault successfully; validate
   the schema, required password and exact portable/secret remote-name set.
2. Canonicalize an existing regular target config; reject symlinks and Unix
   hardlinks. Acquire a permanent sidecar inode using std::fs::File::try_lock.
3. Read the target into a SensitiveBytes buffer and classify it as either rclone
   encrypted (`RCLONE_ENCRYPT_V0`) or ordinary UTF-8 plaintext. Unknown rclone
   encryption headers are rejected.
4. Run read-only `rclone config dump` validation before mutation. This validates
   all target crypt types/backing options and, for encrypted configs, also proves
   the current config can be unlocked. Detect exact no-op restoration.
5. Create an exclusive machine-local recovery directory. Stream the original
   bytes directly into age as `rclone-config.snapshot.age`; decrypt that snapshot
   and compare it byte-for-byte with the original before any update. The journal
   stores only format and BLAKE3 digests.
6. Encrypted-config path: copy only ciphertext into `candidate.conf`, run
   `rclone config update ... --no-obscure` against that encrypted stage, and
   verify all restored obscured values before live replacement.
7. Plaintext-config path: do NOT call rclone's persistent config writer because
   rclone Save() creates plaintext sibling temp/backup files. Instead patch only
   `password` / `password2` in memory, reject ambiguous duplicate sections/keys,
   preserve other lines, and keep the candidate in SensitiveBytes. No plaintext
   staging or backup file is created.
8. Recheck the live original immediately before commit. Any detectable outside
   modification aborts without replacing it.
9. Encrypted candidates use same-directory ciphertext atomic replacement. A
   plaintext candidate is written directly to the already-plaintext live inode,
   with file locking and `sync_all`, so no second plaintext-at-rest copy exists.
   Existing permissions/ACLs are not reset on the same-inode plaintext path.
10. Verify the live result through rclone and compare its bytes with the prepared
    candidate. If verification fails, restore the original and verify it byte for
    byte. A failed plaintext write also triggers an immediate in-memory rollback
    attempt; recovery is retained unless restored bytes and durability sync are
    both verified.
11. Retain the age snapshot and journal if rollback cannot be verified; refuse
    another restore. `recover_interrupted` restores only a current file matching
    the recorded candidate and refuses an unrelated third-party version.

### Why plaintext restore does not invoke `rclone config update`

For an unencrypted config, rclone's normal Save implementation writes a new temp
config and an old-config backup in the same directory before renaming. Those are
additional readable credential copies. Therefore the original B4 command rule is
kept for the encrypted-config path, where those files contain ciphertext, while
the plaintext path uses the deliberately narrow in-memory editor above. This is
a security-driven exception, not a general INI writer: only the two crypt secret
keys may change, and rclone remains the authoritative pre/post validation reader.

The plaintext direct-write path cannot provide the same power-loss atomicity as
the ciphertext rename path without creating another plaintext filesystem object.
The age snapshot and journal are created and verified first, and same-process I/O
failures attempt rollback. After a hard crash that leaves a file matching neither
recorded digest, automatic recovery refuses to guess whether the bytes are a
partial rpool write or an independent edit; the encrypted recovery material is
retained for explicit/manual resolution.

## B6 real-tool verification harness

B6 now includes ignored Rust integration tests in
`src/config_sync/b6_integration_tests.rs`. They use only fresh temporary
directories and explicit `--config` paths, so they do not use the user's normal
rclone configuration. The tests are source-complete but have NOT been executed
in this environment.

The real-tool scenarios cover:

- single crypt remote export -> age vault -> restore;
- multiple crypt remotes with independent keys;
- one source remote with `password2` and one without it;
- byte-for-byte equality of restored obscured `password` / `password2`;
- inability to read the existing encrypted sentinel with placeholder keys;
- successful decryption of the original encrypted sentinel after restore;
- repeated restore returning `NoChanges` without changing config bytes;
- repeated export preserving the exact logical obscured secret bundle and portable crypt definitions;
- generated `password` / `password2` values remaining pairwise independent across tested remotes;
- wrong and missing age identities leaving the target config unchanged;
- authenticated failure for a corrupted `rclone.age`;
- invalid secret schema and missing required password failing before mutation;
- portable/secret remote-name mismatch failing before mutation;
- target `type != crypt` failing before restore mutation;
- no unexpected persistent sibling file after a plaintext restore except the
  permanent cooperative `.rpool-lock` sidecar.

The deterministic post-commit failure/rollback cases remain in the B5 mock
transaction tests, where faults can be injected at exact state-machine phases.
B6 deliberately tests the external rclone/age boundary instead of trying to
make a real executable fail at a nondeterministic commit point.

The B6 tests are `#[ignore]` by default. Optional executable overrides are
`RPOOL_TEST_RCLONE_BIN`, `RPOOL_TEST_AGE_BIN`, and
`RPOOL_TEST_AGE_KEYGEN_BIN`. After a normal build succeeds they can be invoked
manually with `cargo test b6_real_ -- --ignored --test-threads=1`.

## Remaining limits and gates

- B6 runtime execution is still pending on a machine with rclone, age, and
  age-keygen. The source harness exists, but no runtime success is claimed.
- B7 CLI orchestration is connected through top-level `rpool export` / `rpool import`.
  The legacy `rpool config export/import` path remains JSON-only and fails closed
  on crypt-aware bundles so it cannot silently skip secret restoration.
- The B7 artifact has fixed `config/portable-config.json` and `secrets/rclone.age`
  locations. The portable JSON stores only a BLAKE3 digest of the ciphertext; a
  mixed/stale JSON-vault pair is rejected before age decryption or mutation.
- Import performs package, vault, identity, target-rclone, schema, name, type,
  and structural preflight before changing local settings. Portable rpool settings
  are rolled back if the subsequent crypt transaction fails.
- Newly generated 1024-bit password/password2 values remain an internal provisioning
  primitive. `rpool export/import` never rotates existing crypt keys or invents a
  password2 for an existing remote. A future remote-provisioning UI/command must
  still check for existing encrypted objects before applying generated keys.
- No auto-rotation or filling in missing password2 on an existing remote.
- A base64 shape check does not prove a value is genuinely obscured or provide
  authentication. No crypt password decoding API was added.
- Obscured restore values still appear in rclone subprocess arguments. They are
  not logged, but a sufficiently privileged local process can inspect them.
- The sidecar lock coordinates rpool restores, not independent rclone/editor
  writers. Digest checks detect many conflicts but are not a portable atomic
  compare-and-swap with non-cooperating processes. Keep external writers stopped.
- Windows/macOS/Linux behavior has NOT been runtime tested. File handles close
  before replacement; read/write handles are used for candidate sync. Unix file
  modes are preserved. Windows custom ACL equivalence is not established by
  std::fs::Permissions and remains a platform validation item.
- File data is synced; Unix parent directories are synced. There is no claim of
  equivalent directory-fsync/power-loss durability on Windows or all network FS.
- std file locking requires Rust 1.89 or later; dependency MSRVs may be higher.
- Direct-child timeouts do not claim process-tree isolation from arbitrary
  malicious executables or age plugins. Tools and local config directories must
  be trusted; same-user arbitrary filesystem attacks are out of scope.
- A crash can retain encrypted candidate/rclone backup files under the dedicated
  local recovery directory. Do not include that directory in portable exports.

## Static-only acceptance

The source audit checks Rust string/comment/delimiter balance, reachable mod
resolution, Cargo TOML/lock consistency, obsolete-path safety, forbidden output
patterns and the ordering/guards in the restore pipeline. These are NOT Rust
parsing/type checking, cryptographic verification or executed tests.

55 Rust test functions are now present in the Crypt Secret Portability fragment.
Ten are ignored B6 real-tool integration tests; the transaction tests continue
to use explicit fake encrypted fixtures for deterministic fault injection. None
of the Rust tests has been executed in this environment.

Do not run cargo check/build/test unless the user requests it. B1-B7 source work is
complete. The user requested a full-project ZIP for this checkpoint rather than the
normal changed-file-only patch ZIP. `scripts/update-cleanup.nu` remains included.
Runtime B6 execution is still pending and must not be claimed as completed testing.

## Cleanup

Retired src/models/sync_config.rs is added to the explicit cleanup list. Four
already-listed obsolete GUI sources present in the uploaded ZIP were removed
from this worktree, including the conflicting maintenance/integrity.rs.
No legacy plaintext secret files were identified in the input archive, so no
invented secret-cache paths were added. Never sweep recovery snapshots or age
identities from the update-cleanup script.

## Primary references reviewed

- https://rclone.org/commands/rclone_config_update/
- https://rclone.org/commands/rclone_config_encryption_set/
- https://raw.githubusercontent.com/rclone/rclone/master/fs/config/configfile/configfile.go
- https://raw.githubusercontent.com/rclone/rclone/master/fs/config/crypt.go
- https://doc.rust-lang.org/std/fs/struct.File.html
- https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html
- https://github.com/FiloSottile/age
