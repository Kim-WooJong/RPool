# Step 4 — Isolated LocalBackend

2026-09-23. Implemented and tested on macOS / Darwin arm64.

## Scope

`src/storage/local/{mod,tests}.rs` implements the existing `StorageBackend` behind
`cfg(all(test, unix))`. Constructor owns a fresh private `TempDir`; arbitrary
existing roots and production archive routing are not exposed. Windows support
is not implemented (no unchecked reparse-point fallback). Linux is not executed.

The Unix dev dependency `rustix 1.1.5` with `fs` was promoted from an existing
transitive lockfile package. Cargo reconciled Cargo.lock offline; subsequent
test/build commands pass with `--locked --offline`. No production dependency or
application version was added/changed.

## Isolation and write semantics

- Root and each parent are retained directory descriptors. Component traversal
  uses `openat(DIRECTORY | NOFOLLOW)`, including reopening newly created parents.
- Local validation rejects empty/dot components, colons, control characters and
  reserved staging names. Shared ObjectKey and legacy addresses are unchanged.
- Leaf reads use NOFOLLOW and NONBLOCK, then require a regular file. Existing
  directory/symlink destinations are rejected rather than replaced as objects.
- Stage files use mode 0600, exclusive creation, reserved monotonically allocated
  names and descriptor-relative cleanup. Parent directories are mode 0700.
- Source data is streamed in 64 KiB chunks; errors/cancellation before publication
  preserve an existing destination and remove the staging file. Empty parents may
  remain after a failed new write.
- Completed files are published with same-directory renameat (overwrite) or
  linkat (atomic no-clobber create). No exists-then-rename conditional-create race.
- Successful commits return success, not a misleading postcommit cancellation.
- File sync is performed before publication. Directory publication is NOT
  advertised as power-loss durable. Cleanup failure during Drop is best-effort
  and can leave a reserved staging link; it does not roll back a committed object.
- Reads clip at EOF, with accurate byte receipts and bounded-prefix read_all.
  A held read descriptor survives backend overwrite; no cross-call version pinning.

**Boundary:** the private root must be exclusively owned. Descriptor-relative
NOFOLLOW blocks symlink redirection, but is not a guarantee against a hostile
same-UID process relocating already-open directories outside the root, injecting
hardlinks, or changing mounts. No canonicalize-based TOCTOU claim is made.

Capabilities support read/range/stream/write/overwrite/delete/conditional-create
and atomic replacement, with ProcessLocal scope. Conditional update/delete,
version pinning, durable-after-write, listing, native copy and rename remain
Unsupported. No distributed or cross-process CAS claim.

## Executed validation

User explicitly enabled builds/tests in this step, superseding the earlier
static-only workflow. Toolchain: cargo 1.96.0-nightly, macOS arm64.

- Before changes: `cargo test --locked --bin rpool`: 82 passed, 0 failed, 10 ignored.
- Local targeted tests: 10 passed, 0 failed.
- Final `cargo test --locked --offline --bin rpool`: **92 passed, 0 failed,
  10 ignored**, 102 total. Includes all prior MemoryBackend tests.
- `cargo build --locked --offline --bin rpool`: **passed**.
- rustfmt applied only to the new LocalBackend files.
- Expert design + concrete static review: no blockers within the ownership scope.
- Existing Rust files preserved except storage/mod.rs module wiring, checked by
  pre-Step-4 SHA-256 comparison. Manifest, commands and crypt code unchanged.

Ten Local tests cover registry/basic I/O, EOF/limits, failed source cleanup,
atomic publication visibility through reentrant reads, cancel/deadline/sink
errors, path aliases/directory collisions, leaf/intermediate/dangling symlinks,
parent symlink swap during staging, competing conditional creation and honest
capabilities, and Unix socket special-file rejection. All fixtures are temporary.

Initial FIFO fixture compilation failed because rustix mknodat/mkfifoat exclude
Apple. Replaced it with a std Unix socket fixture; FIFO-specific nonblocking
runtime behavior remains unverified. No extra system tool/libc dependency added.

Remaining: 10 explicitly ignored rclone/age B6 integration tests were not run;
no real account/cloud action or GUI session was started. Test build has 6 existing
GUI warnings; ordinary build has 16 warnings in existing GUI/crypt-related code.
These warnings were not suppressed or expanded into unrelated cleanup.
macOS tests/build do not establish Linux/Windows or release readiness.

Next: Step 5 RcloneBackend adapter. Full M1/release remains incomplete.
