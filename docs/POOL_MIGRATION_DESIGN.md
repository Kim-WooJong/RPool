# Pool change migration: design and status

User request: when an account is removed or the pool configuration changes,
move the data onto the remaining storages automatically. First compute and
show what moves and how long it takes, record progress in the cloud so any PC
can resume, and collect the files that cannot be recovered so the user can
see them.

Status (2026-10-01): phases 1–4 are implemented (archives, server-side
copies, the drive with epoch adoption, cleanup). The drive is the v6 pool-sync
drive, the only drive mode. Phase 1 (WP0–WP6 plus the GUI wizard) covers
uploaded archives: `rpool pool migrate plan|run|status|lost|abandon`
and Storage › Account changes › Pool change migration. Differences from
the proposal:

- Lost files are a list only. The user asked for no salvage.
- A relocation writes a full copy under a new archive id. It never borrows
  the old archive's objects, because drain `--delete-source`, drive
  retention and repair could otherwise delete or overwrite them. The plan
  estimates it that way. Phase 2 makes most of that copy server-side (below).
- `run --take-over` resumes entries that a stopped PC had claimed; claims
  otherwise expire after 2 hours.
- `run --parallel N` (GUI: "Archives at once") moves up to N archives at
  once; default 4 (fewer when fewer remain), clamped to 1..=16, and 1 is
  the old one-after-another run. Each archive's own steps stay ordered
  (fingerprint, claim, build, verified, switch, switched) and dispatch
  follows the margin/size order. Each archive keeps the pool's relocation
  `workers`, so up to N x workers transfers run at once: keep N small on
  slow links. Switches (inventory + `replacements.jsonl`) are serialized.
  The stop file stops dispatching; in-flight archives finish. A non-entry
  error (e.g. the journal refusing appends) stops dispatching and fails the
  run once in-flight archives finish.
- Originals already replaced by an earlier migration are skipped by later
  plans.
- Native crypt is not a re-encode reason: the on-disk format is the same.

Verified in Docker (`scripts/linux-docker/pool-migrate-e2e.sh`, native crypt
on and off): remove an account; kill PC A mid-run; PC B resumes with
`--take-over`; the replacements restore byte-identical; the originals are
unchanged; a second loss lists exactly the unrecoverable archives.

Phase 2 (cheaper relocation) keeps the full-copy invariant: the replacement
still gets its own objects under the new archive id. Only how they are made
changed (`migration/relocate.rs`, `RcloneContext::copy_object`):

- A kept shard (same remote, new path) is copied with `rclone copyto` inside
  its crypt remote. When the backend has server-side copy (`rclone backend
  features`: `Features.Copy`; crypt has it when its base has it), the
  provider copies the ciphertext object as-is under the encrypted new name:
  nothing passes through the PC, and the copy stays valid because the crypt
  nonce is in the object header and the key belongs to the remote (native
  crypt writes the same format). Otherwise rclone streams it.
- A readable shard moving to another remote is streamed by rclone (download
  and upload, no temp file).
- A group with an unreadable shard downloads only K readable shards and
  rebuilds and uploads the missing ones; its kept shards are still copied.
- Copies never overwrite: an existing destination is refused, and
  `--ignore-existing` closes the race.
- Verification per shard replaces the unconditional final scan. A
  server-side copy is verified by equal ciphertext size and hash of source
  and destination on the crypt's base (`cryptdecode --reverse` for the
  names, `lsjson --hash`). Every other shard, and any copy without a hash or
  with differing hashes, is read back in full. The manifest is published only
  when every shard has a verification record. Hash equality proves the copy
  is bit-identical to the source object; it does not re-authenticate the
  source (the old archive keeps using that object either way).
  `RPOOL_MIGRATE_FULL_SCAN=1` adds the full plaintext scan;
  `RPOOL_MIGRATE_NO_COPY=1` restores the phase 1 download and upload.
- The planner asks each remote for these features once per plan and
  estimates kept shards with copy and hash as free, with copy and no hash as
  one readback, and unknown remotes as streamed copies. A plan note gives the
  number of shards copied server-side.

Verified in Docker (`scripts/linux-docker/migrate-server-copy-e2e.sh`,
native crypt on and off). Phases 3 and 4 are implemented as well (below).

Phase 4 (cleanup, 2026-10-01) is implemented (`migration/retire/*`,
`rpool pool migrate retire|restore`, GUI step "Clean up" after a complete
migration, strings in `locales/*/migration_retire.json`):

- **What may go.** Only after the migration is complete and not abandoned.
  *Originals* whose winning record is `Switched`, when the replacement passes
  a fresh re-verification (manifest valid, identity and size, every shard
  listed with its size; `--full-verify` reads every shard back), the
  original's current manifest still has the migrated fingerprint, and every
  shard sits inside `<root>/<archive_id>/`. The objects deleted are what the
  root listings show under that folder (manifest replicas and shards).
  *Orphans*: `migrate-<24 hex>` ids that were only ever claimed, lost or
  recorded `Orphan`; an id with a `Verified` or `Switched` record is never an
  orphan (another PC may use it). Objects on accounts that left the pool are
  only deleted with `--include-removed-accounts` (and only when still
  listable); otherwise they are reported as left behind.
- **What is kept.** Drive revisions (`virtual-*`), lost
  entries, anything a fresh reference check finds: every manifest of the
  local inventory and every manifest replica on the listed accounts, all
  drive metadata in the cloud (every v6 event of every generation, incl.
  checkpoints, read only, as `pool browse` reads it), local drive workspaces
  (`--workspace`; the GUI adds running drive sessions), and every archive id
  named by another migration still in progress. A reference is any path
  component or archive id equal to the item. If any of these sources cannot
  be read, nothing is quarantined or deleted.
- **Two steps (fossils).** Quarantine is a journal record only: `Fossil`
  with the exact objects, the PC, the time and the grace period (default 7
  days, `--grace-days`). The objects are not moved: a server-side rename is
  not portable (without `Move` rclone copies and deletes, through the PC),
  it would make the original unreadable during the grace, and restore would
  need another move. `restore` (or GUI "Restore") writes `Restore`. A later
  confirmed run past the grace re-observes everything: a new reference, a
  changed object set or a failed re-verification cancels the deletion
  (`Cancelled`, the item is kept); an unreadable source postpones it.
  Otherwise `Deleting` is recorded (point of no return; a restore that
  arrives meanwhile is checked once more and wins until the first object is
  deleted), the inventory entry is dropped, manifest replicas go first, then
  the shards, journaled in `Deleted` batches, then `Purged`. Interrupted
  deletions resume; a missing object counts as deleted.
- **Mass-delete guard.** Refuses a quarantine or deletion of more than
  `--max-delete-percent` (default 50) of the pool's listed objects or bytes,
  or more than `--max-delete-objects` (default 10 000), before anything is
  written, unless `--force`. The GUI shows the numbers and asks a second,
  explicit confirmation. A migration of every archive often sits right at
  50 % (originals and replacements are the same size), so expect it.
- **Journal.** Cleanup records live in `<migration_id>/retire/<blake3>.json`
  next to `records/`, written and folded like them (content-addressed,
  union of replicas, cached locally), so other PCs see every quarantine,
  restore and deletion and older binaries never see them.

Known limits: other PCs' inventories and unpublished local drive events
cannot be seen; their stale inventory entries of a deleted original stop
verifying (the journal says why). Backend trash/versioning may delay the
freed quota.

Phase 3 (drive and epoch adoption, WP7) is implemented (2026-10-01; unit
tests only, see "Phase 3 as implemented" below; not yet verified with real
rclone remotes or several PCs).

## Phase 3 as implemented: drive migration and epoch adoption

The drive (v6 pool-sync events) is migrated file by file with the archive machinery and
then **adopted** into a new metadata epoch. No workspace is needed: the
source is read from the cloud records, and any PC can run, resume or adopt.

**Plan** (`migration/drive_plan.rs`, `drive_source.rs`,
`mount/drive_generation_read.rs`).
`pool migrate plan` includes the drive unless `--no-drive` (GUI: "Include the
drive", on by default). The current generation is chosen as `pool browse`
chooses it, but adoption-aware (`drive_generations::effective`). Its visible
files (and conflict copies) are read without a workspace by collecting and
projecting the events (checkpoints included). Each file's payload manifest is
classified by the archive planner (`plan_entry`, same listings, probe,
losses, server-side copy estimate). Files that need no move are kept: the
new generation references their existing archive (pool sync does not delete
payloads, see the answers below). The drive plan (`drive-plan.json`, no
manifests) is published before `plan.json`, with its own counts, bytes, ETA
(`speed::estimate_seconds`) and a quota check of archives and drive together.
(The former bootstrap budget check is gone: a new PC opens a large drive from
metadata checkpoints.) The target epoch is
deterministic, `blake3("rpool-migration-drive-epoch-v1", migration_id)`.

**Run** (`migration/drive_run.rs`). After the archives, `pool migrate run`
writes `drive-freeze.json` (the source generation is frozen), reads the
source again and runs the planned entries whose revision is still current
through the unchanged state machine (`execute::run_core`: claims, leases,
`--take-over`, `--parallel`, stop file, lost/unknown), with records keyed
`drive-<blake3(path, revision)>`. Builds use `relocate` / `reencode_manifest`
under new ids (`virtual-<random>` for drive files, `migrate-*` for archives). "Switched" only means the new archive is ready: nothing in the drive
changes. Files changed since planning, and files not checked at plan time,
are left to the adoption.

**Adopt** (`migration/drive_adopt.rs`, `mount/drive_generation_write.rs`).
`pool migrate adopt --id <id> [--accept-lost] [--workspace <ws>]` (GUI step
"Adopt drive"):
1. waits until the freeze is `SETTLE_SECONDS` (5 min) old, longer than a
   mounted PC goes without re-checking the fence (2 min);
2. reads the source again; every file whose revision has a `Switched` record
   uses its new archive, kept files their archive; changed or unchecked
   files are classified and run now (catch-up);
3. refuses while any file is unresolved (claimed by a running PC, provider
   error), and refuses lost files unless `--accept-lost`;
4. publishes deterministic records to `<root>/epochs/<epoch>/` on every
   new-policy remote (one parentless event per file), reads the new generation back like a fresh
   PC and compares it, and only then writes `drive-adoption.json` (the
   commit point). Re-running is idempotent; a second PC writes identical
   records.

**Every PC afterwards** (`mount/adoption_fence.rs`). A new pool-sync
workspace is initialized on the newest adopted epoch of its protocol, so a PC
without the old workspace opens the migrated drive directly (this is what
account recovery could not do). A workspace on the superseded generation is
refused at mount with the command to switch it; `adopt --workspace` renames it
to `.<name>.migration-backup-<epoch>` and first exports changes that were
only local (unpublished sealed writes via the existing spool recovery,
listed with deletions and unpublished commits). While a generation is
frozen, a mount may run but `VirtualDrive::sync` refuses to publish (changes
stay in the spool); the check is cached for 2 minutes per workspace. A failed
check also refuses publication. "Apply pool changes" from a frozen or
superseded workspace is refused (it would fork the drive). `pool browse`
shows the adopted generation and hides superseded ones and partially
published migration epochs. `abandon` lifts a freeze and is refused after
adoption. Nothing is deleted anywhere; the source generation stays readable.

**What is not migrated** (said precisely in the plan notes and by
`adopt --workspace`): writes that exist only on some PC (not uploaded and
published before the freeze took hold) — they stay in that PC's workspace
and are exported when it is switched; previous versions (only the visible
files and conflict copies are carried over; history stays in the source
generation); empty directories (directories are local only); files
written by a PC that kept publishing without checking the fence (an offline
PC that never re-checks before reconnecting is caught at its next sync or
mount, but anything it published to the frozen generation after the
adoption read is not in the new one).

**Interfaces for phase 4 (retire)**: drive records use `drive-*` entry keys
(`drive_model::is_drive_key`), not archive ids; their `new_archive_id` is the
new drive payload, which an adopted generation references. Retire must skip
drive keys, never delete `virtual-*` objects, and treat kept archives as
still referenced by the new generation.

**Needs real validation**: rclone crypt remotes (native crypt on and off), two PCs (one mounted on the old generation during run and adopt,
one without a workspace opening the adopted drive), interrupted adoption,
and the bootstrap of a large drive.

## Flows that existed before `pool migrate` (still available)

| Flow | Code | Limits |
|---|---|---|
| `provider drain` | `commands/provider/drain.rs`, `provider/migrate.rs` | One manifest, one from→to remote. The source must still be readable; it copies bytes and never rebuilds. No plan, ETA or resume. |
| `pool plan-reprocess` / `reprocess` | `pool/reprocess.rs`, GUI `storage/reprocess.rs` | Needs an explicit manifest list. Always a full restore plus re-`put`. ETA uses rates the user types in. Receipts are local only. Handles K/M and degraded sources. |
| `mount --apply-pool-changes` | `mount/pool_transition.rs` | Drive only. Makes a new metadata epoch and copies the locally known view. All or nothing if bytes are missing. No lost-file list. Local journal. |
| `--account-recovery-from` | `mount/account_recovery.rs` | Copies the drive into a new pool. Local per-file report. |

Scrub and repair (`maintenance/*`) already classify every shard as ok,
missing, bad size, corrupt or error, and never count a provider error as an
erasure. `repair_group` rebuilds only onto the same remote and object.

### Constraints

- `pool set` overwrites the policy and keeps no history. Actual shard
  locations are the only trustworthy "old policy".
- The archive inventory is local. Other PCs must list the manifest replicas in
  the cloud to see all archives.
- Drive revisions are immutable archives (`virtual-*`) whose manifests
  are embedded in events. Moving
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
   objects after a final verify. It never touches `virtual-*` or borrowed
   objects.
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

(Original proposal; see "Phase 3 as implemented" for what was built and why
it does not go through `pool_transition`.)

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
- `rpool pool migrate retire <pool> --id <id> [--confirm] [--step all|quarantine|delete] [--grace-days 7] [--include-removed-accounts] [--full-verify] [--max-delete-percent 50] [--max-delete-objects 10000] [--force] [--workspace DIR]... [--json]` (dry run without `--confirm`)
- `rpool pool migrate restore <pool> --id <id> (--item <archive_id>... | --all)`
- `rpool pool migrate abandon --id <id>`
- `pool set` hints when a change affects stored data.

**GUI**

- A Storage › Account changes wizard: Plan → Review (counts, bytes, ETA,
  quota, drive warning) → Run (progress, pause, resume) → Lost files (Retry,
  Salvage, Accept) → Clean up (per-account summary, quarantine, countdown,
  restore, permanent deletion, guard confirmation).
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
- **WP7 Drive:** done as `migration/drive_*`, `mount/drive_generation_*`,
  `mount/adoption_*` (adoption publishes
  the new epoch directly instead of going through `pool_transition`, which
  needs the original workspace and re-uploads every file; see "Phase 3 as
  implemented").
- **WP8 GUI:** `gui/screens/storage/migration.rs` plus the wizard, banner and
  Pools hook.
- **WP9 Tests:** `scripts/linux-docker/pool-migrate-e2e.sh`.

Phases:

1. Archives: relocate with full copy, re-encode, cloud journal, lost list.
2. Borrowing relocation, salvage, measured speeds.
3. Drive (WP7) and the GUI wizard. Done (unit tests; real-remote validation
   pending).
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

Answered from the code (2026-10-01, phase 3):

- **Shard hash.** `Shard.blake3` is the hash of the shard's plaintext bytes:
  `StorageReader::verify` stats and reads the object through the crypt
  remote. Verbatim copies across crypt remotes are therefore checked by a
  plaintext readback; server-side copies additionally by ciphertext hash on
  the crypt's base (phase 2).
- **`get` with a removed remote missing from the config.** rclone fails that
  shard's reads ("not configured"); the read counts it as unavailable and
  decodes from K other shards when they exist. The planner never calls such
  a remote (`RemoteListing::NotConfigured`, counted as `remote_removed`), and
  `pool_transition` passes removed remotes as excluded
  (`StorageReader::rclone_with_excluded_remotes`).
- **Adopt path.** None existed: an epoch was recorded only in the binding of
  the workspace that applied the pool change, a new workspace always opened
  the original generation, and `pool browse` guessed the newest generation by
  record time. Phase 3 adds the adoption marker in the cloud journal and makes
  new workspaces, browse and old-generation workspaces follow it.
- **GC of borrowed shards.** Pool-sync payloads are not deleted by the drive
  itself, and `provider drain --delete-source` refuses `virtual-*` sources, so
  a new generation may reference an unchanged archive. Any drive data
  reclamation (see `DRIVE_HISTORY_DESIGN.md`) must therefore treat every
  generation's events as references, as `retire` does.
