# Step 6 — Backend-neutral read, verification and restore

## Implemented scope

`StorageReader` resolves runtime bindings through `BackendRegistry` and calls the
real `dyn StorageBackend` boundary. Get, status, verify, scrub scanning, manifest
loading/recovery/replica verification and parity reconstruction use this service.
Quota remains a separate administrative operation; repair writes remain Step 7.
Legacy compatibility read helpers delegate to the same implementation.

Legacy addresses remain exact strings, bound at runtime to safe synthetic keys.
They are not normalized or validated as new native ObjectKeys. Local manifest
file precedence is preserved, including Windows-style path considerations.
No persisted model/schema, manifest hash domain, crypt-portability code or Cargo
file changed in this step (SHA-256 comparison against the pre-step snapshot).

Metadata reads enforce a 64 MiB cap and reject short/inconsistent data. Shard
reads verify byte count and hash and request one extra byte to detect oversized
objects. Only NotFound and CorruptData permit parity recovery; authentication,
permission, cancellation and exhausted operational failures propagate. Missing
runtime bindings/backends are InvalidInput, not evidence of remote data loss.
Scrub refuses automatic repair while provider read errors remain unresolved.

Restore keeps the existing offsets, Reed-Solomon stripe layout, short-tail padding
and virtual zero slots. Resume drops unknown completed indexes and rehashes claimed
ranges. Parity temporaries have a unique owned directory; file handles close
before cleanup, including early returns, to accommodate Windows file semantics.

## Review and verification

Two focused expert reviews covered read injection/address compatibility and
restore/error policy. Their blocking finding was routing errors being classified
as missing objects; this was corrected and covered by a regression test.

Executed on macOS arm64:

- `cargo test --locked --offline --bin rpool`: **117 passed, 0 failed, 10 ignored**.
- `cargo build --locked --offline --bin rpool`: **passed**.
- 12 new tests cover actual trait injection, plain/RS restore, resume revalidation,
  short/corrupt/oversized reads, typed operational failures, retry, metadata caps,
  manifest recovery, command cores and exact legacy addresses through fake rclone.
- Test profile reports 6 warnings; normal build reports 27, including unused
  compatibility bridges and pending integration surface. Cleanup remains Step 11.

Windows is the primary deployment target; Windows/macOS/Linux were considered in
implementation. Only macOS was executed here. The three-OS CI file remains local
and unexecuted. Ten real-tool integration tests remain ignored; no actual rclone,
cloud account or remote mutation was exercised. This is not full-M1 certification.
A group error can leave successful sibling downloads unjournaled, causing safe
redundant downloads on resume rather than incorrectly recording completion.

## Next

Step 7: write/resume/repair/migration injection, preserving encryption binding,
replacing size-only skips with shared hash verification, and preserving safe
manifest/replica/source-deletion ordering.
