# Step 5 — RcloneBackend / portable subprocess boundary

2026-09-23. Source implemented; macOS arm64 build and synthetic tests passed.
Windows is the primary deployment target; Windows/macOS/Linux must be considered.

## Ownership and compatibility

- `storage/rclone/mod.rs` owns command construction, explicit executable/config
  context, frozen environment, crypt policy and raw-address primitives.
- `storage/rclone/process.rs` owns direct-child lifetime, bounded stream buffers,
  bounded stderr retention, context checks and sanitized failure classification.
- Existing cat/stat/probe/copy/upload/download/verify free functions are bridges.
  Quota/config discovery also use the owning adapter runner. There are no remaining
  production `Command::new` calls elsewhere inside storage.
- `storage/transfer.rs` owns legacy hashing, expected-length checks, output offsets
  and retry policy; adapter primitives do not implement the archive hash policy.
- `RcloneBackend` implements the actual existing trait and is exercised through
  BackendRegistry. Legacy raw addresses use the same primitives without converting
  through ObjectKey, so exact Unicode/backslash/colon/percent strings are preserved.
- ConfigSelection distinguishes inherited selection from an explicit PathBuf.
  Existing bridge signatures preserve inherited behavior; command-level explicit
  registry composition/DI is deferred to Step 6/7, not represented as complete.
- Production manifests, command files, crypt portability files, Cargo.toml,
  Cargo.lock and version 0.5.15 are unchanged.

## Crypt gate and errors

Fresh config inspection precedes every write/copy destination operation. The
executable-only global cache was removed. Config inspection and I/O share the
same captured environment/config selection. Non-crypt, disabled/malformed data
encryption policy and policy-changing environment overrides are rejected.
No public unchecked write primitive or bypass boolean was added.

Config JSON stdout is capped at 8 MiB; raw config/command/stderr or callback error
text is not echoed in public diagnostics. Environment overrides are rejected by
name only (values never logged). Fresh checks do not eliminate external edits to
the config file between inspection and use; no immutable binding guarantee is
claimed. Legacy deletion remains deletion, not an encrypted write operation.

Stat and probe use one classifier. Documented rclone missing exits 3/4 become
NotFound, after explicit authentication/permission evidence. Generic failures,
missing config-section diagnostics with generic exit status, and malformed JSON
do not become Missing. Directories are errors at the legacy size bridge.

After a mutation subprocess starts, interrupted I/O/cancellation/failing status
is conservatively UnknownOutcome. Legacy retries do not repeat UnknownOutcome.
Rclone mutation retry flags are set to one attempt. Reads can retry typed transient
failures; a completed failed mutation is never assumed not to have happened.

## Streaming and cancellation

- Native Command/OsString/PathBuf arguments, no shell interpolation. Windows uses
  CREATE_NO_WINDOW; spaces, Unicode and backslashes are not shell-split.
- Caller owns borrowed Read/Write callbacks (no added Send requirement). Fixed
  64 KiB pipe buffers, concurrent stderr drain retaining at most 16 KiB, and a
  scoped supervisor that kills a direct child on context expiry/cancellation.
- A guard installed before worker spawning kills/reaps on every post-spawn exit,
  including panic unwinding. Output cap/sink failure kills before waiting for EOF.
- Arbitrary user callbacks cannot be forcibly interrupted. Descendant process
  trees/handles are not managed (no Windows Job Objects/Unix process-group claim).
- Requested range output is independently bounded; CLI signed-int64 range limits
  are rejected rather than wrapped. Empty reads confirm existence with stat.
- Trait read_all(None) returns the whole object; explicit limit returns a prefix.
  The legacy metadata bridge separately caps full metadata at 64 MiB and errors
  on overflow, never silently truncates. General streaming has no whole-object
  buffering in Rust. Rclone itself may stage unknown-size uploads internally.
- Version pinning, conditional writes, atomic rename, guaranteed native copy and
  bounded listing are Unsupported. Legacy copyto remains a guarded compatibility
  operation, not a claim of native cross-backend copy capability.

## Validation

- `cargo test --locked --offline --bin rpool`: **105 passed, 0 failed, 10 ignored**.
- `cargo build --locked --offline --bin rpool`: passed.
- 12 adapter tests + one transfer retry test added. The fixture is a Rust executable
  compiled with rustc at test time, not a Unix shell script. Tests cover executable
  and config paths containing spaces, exact legacy addresses, registry injection,
  stream/range output, errors, fresh policy/config isolation, blocked writes/copy,
  lost acknowledgement, stalled stdin/stdout, bounded output/stderr floods,
  source/sink failures, legacy transfer hashes/offsets and retry suppression.
- Initial 150 ms fixture deadlines expired during pre-mutation policy inspection.
  They were corrected to generous deadlines, and post-spawn mutation timeout is
  tested directly at the runner boundary. Pre-spawn timeout remains Timeout.
- Expert reviews contributed process-guard placement before thread creation,
  metadata-cap separation from trait semantics, and honest crypt/process limits.
- Test profile: six existing GUI warnings. Normal build: 18 warnings (16 existing
  plus two pending Step6 integration warnings for explicit File context and backend
  constructor). No blanket warning suppression was introduced.
- SHA-256 comparison: existing-file changes are confined to ten storage bridge/
  admin/module files. All existing non-storage Rust and both Cargo files preserved.

**Executed OS:** macOS arm64 only. Windows and Linux execution are pending.
`.github/workflows/storage-cross-platform.yml` adds Windows/macOS/Linux build and
test jobs; it was written locally, not published or executed. The Unix-only Local
prototype from Step4 still does not provide Windows reparse-safe local storage.

No installed real rclone was found on the checked executable search path. Fake
process tests verify our boundary, not actual provider/rclone behavior. Ten B6
real rclone/age integration tests remain ignored. No cloud/account action or GUI
session ran. Full migration and release readiness remain incomplete.

Next: Step 6 — inject backend-neutral read/verify/restore paths.
