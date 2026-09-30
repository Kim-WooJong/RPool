# Pool change migration: design (proposed, 2026-09-30)

User request: when an account is removed or the pool configuration changes,
move the data onto the remaining storages automatically. First compute and
show what moves and how long it takes, record progress in the cloud so any PC
can resume, and collect the files that cannot be recovered so the user can
see them.

Status: design only; nothing is implemented yet. Produced by a read-only
architecture review and pending the user's decisions (see the end).

## What exists today (five separate flows)

| Flow | Code | Limits |
|---|---|---|
| `provider drain` | `commands/provider/drain.rs`, `provider/migrate.rs` | One manifest, one from→to remote. The source must still be readable; it copies bytes and never rebuilds. No plan, ETA or resume. |
| `pool plan-reprocess` / `reprocess` | `pool/reprocess.rs`, GUI `storage/reprocess.rs` | Needs an explicit manifest list. Always a full restore plus re-`put`. ETA uses rates the user types in. Receipts are local only. Handles K/M and degraded sources. |
| `mount --apply-pool-changes` | `mount/pool_transition.rs` | Drive only. Makes a new metadata epoch and copies the locally known view. All or nothing if bytes are missing. No lost-file list. Local journal. |
| `--account-recovery-from` | `mount/account_recovery.rs` | Copies the drive into a new pool. Local per-file report. |
| `mount --migrate-excluded` | `mount/workspace_capacity.rs` | Legacy replica path. |

Scrub and repair (`maintenance/*`) already classify every shard as ok,
missing, bad size, corrupt or error, and never count a provider error as an
erasure. `repair_group` rebuilds only onto the same remote and object.

### Constraints

- `pool set` overwrites the policy and keeps no history. Actual shard
  locations are the only trustworthy "old policy".
- The archive inventory is local. Other PCs must list the manifest replicas in
  the cloud to see all archives.
- Drive revisions are immutable archives (`virtual-<intent>`,
  `peer-v7-…`) whose manifests are embedded in events or snapshots. Moving
  their shards in place does not change what readers see, so the drive
  changes only through a new epoch.
- Metadata roots depend on the full remote set, so a remote-set change always
  means an epoch transition for the drive.
- rclone has no atomic create-if-absent. Journals must be content-addressed,
  append-only records.

## Design: `pool migrate`

A new module `src/migration/` owns planning, the journal and orchestration. It
reuses reprocess, maintenance, placement and pool_transition internals.

### Plan

1. **Inputs.** The new policy (saved, or a draft), and the old state read from
   actual shard locations. From now on `pool set` writes a policy snapshot so
   later diffs are exact.
2. **Enumerate.** Local inventory, manifest replicas listed in the cloud, and
   the drive's current view (epoch-aware, as `pool browse` does).
3. **Classify each entry:**
   - `unaffected`
   - `relocate`: same coding, but shards sit on removed remotes, or placement
     is no longer valid.
   - `reencode`: K, M, shard size or native crypt changed (reprocess
     `convert_one`).
   - `lost`: some group has fewer than K readable shards.
   - `unknown`: a provider error. Never counted as lost.

   Adding a remote moves nothing unless `--rebalance` is given.
4. **Probe.** Quick mode lists each remote once and compares sizes. Deep mode
   hashes the affected entries or all entries. A removed account that is still
   readable is copied verbatim, which is cheaper and safer than decoding.
5. **Bytes and ETA.**
   - relocate: download K×shard per affected group, or shard only when the
     source is readable; upload and readback the moved shards.
   - reencode: the reprocess formula, moved into a shared `pool/estimate.rs`.
   - Speeds are measured with a short upload/download/delete benchmark per
     remote under `.rpool-sync/bench/`, else taken from the user.
   - ETA is modelled per remote and shown as a range.
6. **Quota.** A conservative placement-aware preflight refuses to start when
   the new bytes do not fit.
7. **Output.** A frozen `plan.json` published to the cloud journal.

### Execute

1. Preflight, then re-check the frozen fingerprints.
2. Process the lowest redundancy margin first. Drive items go last.
3. For each entry:
   1. Build the replacement.
      - `relocate_shards`: `repair_group` generalised to take a destination,
        plus `placement::assign_replacement_slots` under the failure-domain
        rules.
      - `convert_one` for re-encoding.
   2. Verify with a full readback.
   3. Publish the manifest replicas.
   4. Write journal `verified`.
   5. Switch: archives go to the inventory and manifest; drive items feed
      pool_transition.
   6. Write journal `switched`.
4. Always create a new archive id (copy, then switch).
   - Phase 1 makes full copies.
   - Later, relocation may borrow unchanged shards. The manifest is then
     marked, and nothing borrowed is ever deleted.
5. Never delete during `run`. A separate `retire --confirm` removes old-only
   objects after a final verify. It never touches `virtual-*`, `peer-v7-*` or
   borrowed objects.
6. Resume by folding the journal:
   - `switched` entries are skipped after a quick re-verify.
   - `verified` entries are re-verified, then switched.
   - Entries with no record get a fresh attempt.
   - Leftovers are recorded as orphans.
   - The stop file is honoured.

### Cloud journal

- **Path.** `<remote>/.rpool-sync/migrations-v1/<scope>/<migration_id>/`,
  with the same scope hash as pool sync. Replicated to every new-policy remote
  and to the old ones while they still work. Encrypted like events, with no
  secrets inside.
- **Records.** `plan.json`, `policy-snapshot.json`, `activated-epoch.json`,
  `records/<blake3>.json` and `lost/<entry>.json`. A record looks like
  `{entry, state: claimed|verified|switched|lost|unknown|orphan|abandoned,
  attempt_id, pc_id, new_manifest…, ts}`.
- **State.** The union across replicas, folded monotonically: the highest
  state wins and duplicates are harmless.
- **Several PCs.** Advisory per-entry `claimed` leases. Correctness never
  depends on them: if two replacements get verified, the deterministic winner
  is the smallest `(ts, attempt_id)` and the other becomes an orphan. That
  costs quota, not data.
- **Resume from another PC.** Needs the same pool config and keys
  (config_sync bundle). `pool migrate status` lists the migrations in the
  cloud.

### Lost files

```
LostItem { migration_id, kind: archive|drive_file, archive_id, original_name,
  drive_paths[], revision_id, size, status: lost|unknown,
  groups: [{group, byte_range, required_k, available,
            missing: [{index, remote, reason: remote_removed|missing|bad_size|corrupt|provider_error}]}],
  detected_by: quick|full, detected_at }
```

Stored in the cloud journal under `lost/`, cached locally, and exportable as
JSON or CSV. Actions:

- **View:** path, size, and missing groups and shards with reasons.
- **Retry** `unknown` items.
- **Reattach** a removed account and re-probe.
- **Salvage:** healthy groups are contiguous byte ranges, exported as
  `<name>.partial` plus a hole map.
- **Accept as lost:** `--accept-lost` lets the drive epoch activate without
  those files. The old epoch backup keeps its references.

### Drive

The orchestrator:

1. Pulls first.
2. Rebuilds the archives behind every current revision as migration
   replacements.
3. Runs the pool_transition logic with them, as it does today with
   `recovery_reprocess_plan`.
4. Writes `activated-epoch.json`.

Other PCs see the marker and offer "adopt the new generation": a fresh
bootstrap that keeps the old workspace as a backup. This adopt path still has
to be built. Writes from a PC still mounted on the old epoch are not migrated;
warn about it and detect it. The plan must also check the 10k-event / 64 MiB
bootstrap budget.

## CLI and GUI (parity)

**CLI**

- `rpool pool migrate plan <pool> [--include archives,drive] [--workspace ws] [--probe quick|full] [--measure-speed | --download-mib-s --upload-mib-s] [--rebalance] [--json]`
- `rpool pool migrate run --id <id> [--workers] [--stop-file] [--accept-lost]`
- `rpool pool migrate status [--id] [--json]`
- `rpool pool migrate lost --id <id> [--json|--csv]`
- `rpool pool migrate salvage --id <id> (--entry N | --all) --output <dir>`
- `rpool pool migrate retire --id <id> --confirm` (later)
- `rpool pool migrate abandon --id <id>`
- `pool set` hints when a change affects stored data.

**GUI**

- A Storage › Account changes wizard: Plan → Review (counts, bytes, ETA,
  quota, drive warning) → Run (progress, pause, resume) → Lost files (Retry,
  Salvage, Accept).
- The existing drain, reprocess, apply and recover cards move under
  "Advanced / manual".
- Pools asks "Plan migration now?" after an edit that affects data.
- A global banner shows an unfinished cloud migration or an adoptable drive
  epoch.

## Risks

- **Data loss.** Deletion happens only in `retire`. The lowest-margin groups
  go first. `unknown` stays separate from `lost`. Cloud manifest listing
  covers archives this PC never indexed.
- **Cost and quota.** Relocating reads K shards per affected group, which can
  mean egress fees. The quota preflight checks space before starting, and a
  run stops cleanly between entries. Orphans are recorded so they can be
  cleaned up.
- **Several PCs.** Leases are advisory only. Idempotent records and a
  deterministic winner keep the result correct. A drive still on the old
  epoch is warned about.
- **Partial failures.** The union fold tolerates partial journals. A remote
  that returns is re-probed. When failure-domain safety is insufficient the
  plan reports it and `--allow-risky` is required.

## Work packages (parallel after WP0)

- **WP0 Contracts:** `migration/{mod,model}.rs` and `pool/estimate.rs`.
- **WP1 Planner:** `migration/{enumerate,classify,estimate,plan}.rs` and the
  `pool set` snapshot hook.
- **WP2 Probe, lost and salvage:** `migration/{probe,lost,salvage}.rs`.
- **WP3 Relocation:** `migration/relocate.rs`, a destination for
  `maintenance/repair.rs`, and `placement/assign.rs`.
- **WP4 Cloud journal:** `migration/journal.rs`.
- **WP5 Speed:** `migration/speed.rs`.
- **WP6 Orchestrator and CLI:** `migration/execute.rs`, `cli/pool.rs`,
  `commands/pool/migrate.rs`.
- **WP7 Drive:** `mount/pool_transition.rs`, `migration/replacements.rs`, the
  adopt path.
- **WP8 GUI:** `gui/screens/storage/migration.rs` plus the wizard, banner and
  Pools hook.
- **WP9 Tests:** `scripts/linux-docker/pool-migrate-e2e.sh`.

Phases:

1. Archives: relocate with full copy, re-encode, cloud journal, lost list.
2. Borrowing relocation, salvage, measured speeds.
3. Drive (WP7) and the GUI wizard.
4. `retire` and orphan cleanup.

## Verification

- **Unit tests:** classification, estimates, lost vs unknown, journal fold
  (duplicates, disorder, two winners), resume from each state, no deletion
  during run, replacement placement, salvage map, quota and event-budget
  refusals.
- **Docker e2e with local crypt remotes:**
  1. Remove one of four remotes (RS 2+1): no loss, ETA shown, kill mid-run,
     resume from a second HOME, bytes identical, drive on the new epoch.
  2. Remove two remotes: exactly the expected lost files, salvage, accept.
  3. Change 2+1 to 3+1.
  4. Add a remote: nothing moves.
  5. Two PCs at once: they converge and orphans are recorded.

## Open questions to verify before implementing

- Whether a manifest's shard hash is of the plaintext (needed for verbatim
  copies across crypt remotes).
- How `get` behaves when a removed remote is missing from the rclone config.
- Whether an "adopt new epoch" path already exists.
- Whether any GC could delete borrowed shards.
