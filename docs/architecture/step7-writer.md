# Step 7 — Verified writes, resume, repair and migration

## Completed behavior

`StorageWriter` owns the shared `StorageReader` routing/context and drives actual
`dyn StorageBackend` writes and deletes. Production construction remains rclone-only:
crypt policy is checked before reuse or write, and the adapter independently repeats
the write gate. Injected synthetic construction is compiled only for tests; no
unencrypted native archive-write activation is introduced.

Data upload, parity upload, manifest replicas, repaired shards and migrated objects
use this service. Existing upload/copy/delete compatibility helpers also delegate
to the service/trait boundary. Reuse requires both expected size and full BLAKE3,
never size alone. Only definite NotFound/CorruptData permit replacing an object;
operational or routing errors propagate. Successful writes require receipt and
source-consumption counts, then full remote hash verification before success.
UnknownOutcome is terminal within the call. Readback failure never restarts mutation.
A later explicitly resumed invocation may independently verify committed content.

`UploadSource` captures a uniquely owned, portable disk snapshot, comparing original
hashes before/after capture with the snapshot hash. All data and parity use that
same snapshot. Original paths still own sidecars, manifest naming and CLI output.
Journal validation checks every plan identity field and current snapshot data hashes;
parity is regenerated. Invalidations are applied only after validation completes,
so an operational failure leaves the journal unchanged. Journal completion follows
verified writes. Changed same-size source resumes rebuild mismatching content.

Migration uses verified logical-byte disk staging, not native server-side copy or
raw backing ciphertext copy across distinct crypt bindings. Ordering remains:
all copied destinations verified → local manifest → all verified manifest replicas
→ source deletion. Partial publication can leave multiple replica generations;
source deletion never follows a failed replica write/readback. No distributed
transaction or automatic remote rollback is claimed.

Repair reads through the same service and verifies destinations after upload.
Encoding/repair staging now uses per-operation owned temporary directories. Handles
are closed before reopening/removal for Windows compatibility. Repair and scrub
command dry-runs no longer save local integrity snapshots; migration dry-run also
avoids publication/deletion.

## Verification

Executed on macOS arm64:

- `cargo test --locked --offline --bin rpool`: **135 passed, 0 failed, 10 ignored**.
- `cargo build --locked --offline --bin rpool`: **passed**.
- 18 new tests: 17 injected service/command tests and 1 fake-rclone crypt policy test.
- Covered same-size corrupt data/parity replacement, full-hash reuse, uncertain
  committed write, failed readback, operational errors, journal/source identity,
  immutable snapshots, repair success/failure/dry-run, migration copy/replica failure,
  replica readback failure before deletion, two distinct backends, command dry-run
  local-state preservation, failed put and changed-source interrupted resume.
- 16 test-profile and 51 normal-build warnings remain, including newly unused legacy
  compatibility wrappers. No warning-free claim; broad cleanup stays Step 11.
- SHA-256 preservation: all models, crypt-portability code, Cargo files and manifest
  hash-domain/validation files unchanged. 18 existing Rust files changed; 3 added.

Expert design/concrete reviews covered write policy/source consistency and
repair/migration publication order. A blocking command-layer dry-run snapshot write
was fixed and regression-tested. The initial test compile exposed a private-module
access issue; a test-only re-export fixed it. The first put integration test also
reached optional local inventory bookkeeping; that bookkeeping was moved back to
the CLI wrapper, and its exact test-created inventory entry was removed while
retaining other entries. Injected put tests now avoid user inventory mutation.

## Limits and cost

Windows remains primary; all new code/tests use portable Rust APIs. Actual execution
here was macOS only. Windows/Linux CI remains local and unexecuted; ten real-tool
integration tests remain ignored. No real rclone/cloud provider was accessed.

Upload needs temporary space for a full source snapshot plus concurrent shard spools
and parity staging. Migration/repair also require disk spools; memory is not used to
buffer whole archive objects. Readback adds remote traffic. Input edits after capture
do not alter the captured archive. Arbitrary adversarial concurrent file mutation,
independent external remote writers and config-edit TOCTOU are not solved by this
single-writer M1 flow. LocalBackend Windows activation remains out of scope.

## Next

Step 8 — separate provider discovery/quota/tool diagnostics and GUI compatibility
management from backend data I/O. Preserve Windows-first, three-OS validation policy.
