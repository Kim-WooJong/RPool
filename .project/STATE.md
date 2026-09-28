# Project State

Updated: 2026-09-28

## Project

RPool is a Rust GUI/CLI sharded archive tool over encrypted rclone remotes.
Canonical source is this Git checkout (`artifacts/rpool`). Build outputs belong in
`projects/rpool/target`, not in this repository. Windows is the primary intended
GUI platform; current executed tests are macOS.

## Current Status

Version 0.6.0 with batched Unreleased changes. Local-first writable mounts and
optional shared revision exchange are implemented and synthetically tested.
No real cloud/multiple-PC/WinFsp integration validation has been performed here.

## Working

- Persistent local workspace, verified archive writeback, retained old versions.
- Optional encrypted shared-root + worker-name GUI/CLI inputs.
- Immutable causal file revisions; deterministic worker-labelled conflict copies.
- Offline edit/edit and edit/delete preservation, causal deletion/recreation.
- Incoming reconciliation with displaced-byte recovery journal and catalog v2.

## In Progress

No unfinished implementation for this bounded shared-namespace task.
Unrelated config-sync/path changes may be present in the working tree; preserve them.

## Next

1. Validate on actual Windows/WinFsp with two PCs and test cloud accounts.
2. If live incoming replacement while mounted is required, design a filesystem
   write barrier/open-handle-aware integration before removing the current gate.
3. Plan shared-history compaction before deployments exceed exchange limits.

## Known Issues

- Mounted mode publishes/fetches revisions only. Incoming files/deletions require
  unmounted reconciliation with no remaining VFS cache files or mount lease.
- Case/prefix collisions of live paths fail closed; no silent overwrite.
- Empty directories remain local-only. Rename is new path plus deletion.
- No history/recovery GC; exchange limits: 10k events, 8 MiB/event, 64 MiB total.
- Shared metadata root is a separate availability dependency. All PCs need the
  appropriate crypt keys and compatible remote aliases for archive manifests.

## Decisions

- Conflict outcome is causal, not timestamp-based last-writer-wins.
- Preserve both concurrent file versions and retain an edit concurrent with deletion.
- Each PC owns its own workspace/cache. Never share/copy an active workspace.
- No remote deletion and no automatic deletion of local recovery/cache data.
- Quiescent local files are required while unmounted reconciliation runs.

## Architecture

- `src/mount/shared_model.rs`: validated immutable events and deterministic reducer.
- `src/mount/shared_transport.rs`: encrypted content-addressed event exchange.
- `src/mount/workspace_shared.rs`: local baselines, publication, incoming apply/recovery.
- `src/mount/workspace.rs`: local catalog, snapshots and verified archives.
- `src/mount/adapter.rs`: owned rclone mount/cache lifecycle.
- `docs/MOUNT.md`: user-facing usage, boundaries and recovery guidance.

## Validation

2026-09-28, macOS:
- `cargo test --locked --bin rpool`: 226 passed, 12 ignored.
- `cargo test --locked --features opendal-prototype --bin rpool`: 237 passed, 12 ignored.
- Default and optional `cargo rustc --locked --release --bin rpool -- -D warnings`: passed.
- Synthetic behavior is not real cloud/WinFsp/FUSE proof.

## Resume

Requested shared namespace/conflict-copy implementation is complete to the stated
deferred-apply scope. Start with `docs/MOUNT.md` and Git status. Do not infer live
multi-machine mount validation from unit tests. Preserve other worktree changes.
