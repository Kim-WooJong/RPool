# Read/write Pool drive

Two modes are available. The GUI defaults to **Online drive** (virtual); the CLI
retains explicit `--virtual-drive` opt-in. **Replica** keeps a full local copy.
The online drive lists the shared metadata namespace without
restoring all files, then downloads/verifies only intersecting shards on reads.
Both modes keep writes on local disk before asynchronous verified cloud publication.
An application save is **not** a completed-cloud-replication acknowledgment.
For the current online-write path and native frontend plan, see
[Online drive write path and native frontend plan](MOUNT_WRITE_ROADMAP.md).

## Account failure / removing accounts while keeping a writable drive

A workspace binds its original pool topology. Editing the pool's remote list is
not an in-place migration: ordinary mounts deliberately reject a mismatch, and
ordinary pool sync still requires all of that pool's metadata replicas.

Use **Storage → Mount → Recover after account removal** to create an independent
writable destination on remaining accounts:

1. Stop the original mount. Keep its workspace, rclone account definitions and
   old cloud data; do not delete or manually edit `virtual.json`.
2. In **Pools**, create a **different pool name** containing only usable accounts.
   Choose coding/placement settings feasible on those accounts. Select this pool
   in Mount and set a **new empty destination workspace**. Use Online drive and
   Automatic pool sync, with Automatic history deletion **off** for the destination.
3. In the recovery panel, select the original online workspace. Enter failed
   **rclone remote aliases**, one per line (for example `failed-crypt`, not a URL,
   password, provider display name or `failed-crypt:path`). These aliases are
   explicitly skipped for source reads and must not belong to the destination.
4. If **Reprocess data** has already finished, use **Use saved plan in mount recovery**
   on that screen, or select that operation's `plan.json`
   in **Completed Reprocess plan**. Exact original-manifest fingerprints and
   completion receipts link each old file to its independent replacement archive;
   filenames alone are never used for matching. Replacement bytes are verified
   again on the remaining accounts, then referenced without another upload.
   Reprocess currently updates the archive library, **not existing mount references**;
   this step connects those completed results to the new writable namespace.
5. Run recovery and inspect `account-recovery.json` in the destination. It reuses
   matching completed Reprocess results or copies unmatched recoverable contents,
   retaining current names, local sealed writes and visible conflict copies; it
   does not grant the new pool ownership of old objects. Destination uploads and
   metadata publication must succeed before a file is reported recovered.
6. Use **Mount read/write** with this destination. New files use the remaining
   accounts. Resume incomplete recovery using the same source and destination;
   never overwrite newer destination edits to make a recovery report look complete.

CLI example (PowerShell, with a new `recovered-pool` already configured):

```powershell
.\rpool.exe mount --virtual-drive --pool-sync --pool=recovered-pool --workspace="C:\RPoolRecovered" --account-recovery-from="C:\RPoolOriginal" --recovery-skip-remote=failed-crypt
.\rpool.exe mount --virtual-drive --pool-sync --pool=recovered-pool --workspace="C:\RPoolRecovered" --mountpoint=R:
```

Add `--recovery-reprocess-plan="C:\path\to\reprocess\operation\plan.json"`
to the first command to reuse completed Reprocess output. Keep the plan directory
and completion receipts intact. Pending edits newer than those receipts are copied
as their current contents, never replaced with an older reprocessed version.

Recovery **does not mount automatically**. This is a new independent current-file
view, not a membership change propagated to old PCs: move other writers to the new
pool deliberately. Source history and local state are retained, not purged.
Old v7 snapshots cannot invalidate successfully copied destination payloads.

**Limits:** the source is its locally known snapshot, not a fresh authoritative
cloud listing. Files/events never synchronized to it, historical versions, partial
writes and dirty native VFS cache are not silently declared recovered. Sufficient
surviving shards/parity, completed Reprocess replacements, or sealed local spool
are required. Original clean-cache files are preserved but not reused by this
recovery operation. An unavailable source
file remains a reported failure rather than becoming an empty file. A destination
with partial recovery can still be used for new files, but is not a complete copy.
Recovery costs download/upload traffic, destination capacity and temporary local
space. No failed account is deleted and no authentication error is automatically
treated as proof that data is absent.

## Capacity display and configuration preview

For **account used/total/free**, coding upper bounds, and placement-aware additional
space, see [Pool capacity](POOL_CAPACITY.md). Storage → Pools can calculate the
current unsaved options without creating a workspace or mounting. The same engine
feeds mount reports. Virtual DAV quotas expire stale samples and reserve queued
upload space; replica OS capacity remains local disk capacity.

Windows Explorer's **1 PB total / 1 PB free** is rclone's unknown-quota fallback,
not the pool's capacity. The online DAV bridge reports known namespace usage and
**zero additional free bytes** while cloud capacity is missing, expired or being
refreshed. A fresh verified sample restores the calculated logical total/free.
Zero free in this state is conservative and does not prove the pool is full;
check the app's capacity status and account errors. Values are logical estimates
after coding/placement and pending reservations, not a sum of provider raw quotas.
Protocol reference: [rclone WebDAV About](https://github.com/rclone/rclone/blob/master/backend/webdav/webdav.go)
and [VFS Statfs unknown-space fallback](https://github.com/rclone/rclone/blob/master/vfs/vfs.go).

## Automatic history collection — private snapshots v7 (NEW workspace)

In the GUI choose **Virtual cloud drive → Automatic pool sync → Automatic history
deletion — v7**, then set **Previous versions per file**. CLI equivalent:

```sh
rpool mount --virtual-drive --pool-sync --pool-retention \
  --pool=my-pool --workspace=/new/persistent/workspace \
  --pool-worker=PC-A --pool-history-limit=10 --mountpoint=/empty/mountpoint
```

Every PC uses its own new workspace, the same pool/remotes/keys, and the **same
history limit**. No designated PC, separate metadata service, or online-PC quorum
is required. All configured metadata storage destinations must be reachable.

- `0`: retain current content; `10`: current + up to 10 previous content revisions
  per resolved file. Conflict originals/candidates and temporary publication
  overlap are additional protections, not counted against ordinary history.
- The limit is fixed for this initial v7 protocol scope. Mismatched policies fail
  closed; changing one PC's config is not a coordinated policy update. Increasing
  the number cannot restore already deleted versions.
- Before retiring a snapshot, RPool copies and verifies its retained closure into
  a **new private ownership domain**. Positive causal successor evidence, not a
  missing directory listing or device timeout, authorizes exact-object deletion.
- Multiple PCs may publish and collect in parallel. Interrupted deletion resumes
  from a checksummed local journal; repeated deletion of missing objects is safe.
- This is eventual payload reclamation, not an instantaneous storage ceiling.
  Copying current/history/conflict bytes needs temporary cloud/local space and
  adds transfers. Backend trash/versioning may delay actual quota recovery.
- Causal metadata, namespace tombstones and nested archive metadata remain; fresh
  bootstrap still has metadata size/count bounds. Interrupted unpublished uploads
  remain conservatively retained and are **not** automatically orphan-swept.

### Moves, concurrent edits, and reads

File identity is separate from both pathname and content identity. A committed
file/folder MOVE publishes one immutable metadata envelope and does **not** upload
or download payloads. History follows that file identity across moves. Pending
writes must finish syncing before moving. Conflicted content/name groups currently
refuse MOVE; moving one conflict alias must not silently move every sibling.

Before admitting an edit, v7 captures/verifies the exact input original locally.
Concurrent ordinary edits preserve the original plus both worker-labelled candidates.
Very stale edits whose original has already expired fail explicitly; recover local
work as a new file rather than silently overwriting current data. Native VFS cache
and rejected/partial spool require the documented recovery procedures below.

Reads resolve the **same semantic revision** through current private copies;
compaction never substitutes a newer file version into an old handle. If that
semantic revision has itself expired, reopen the file. Application `fsync` still
is not an immediate remote-durable commit guarantee. Empty directories remain local.

### Transition from existing data

V7 uses a disjoint `snapshots-v7` metadata namespace and new payload owner IDs.
Existing v6/v5/replica workspaces and objects are **not** adopted or deleted.
Old binaries cannot mutate the v7 namespace through their v6 synchronization path.

To populate v7, copy files from the old drive to the new drive, or explicitly import
selected manifests with `--manifest`. Import restores/verifies bytes and creates
new owned payloads; it never grants ownership of old object references. Remove the
one-time manifest arguments after importing. There is no automatic in-place
migration or legacy-history cleanup. Keep the old workspace intact until you have
verified the copied files.

Synthetic tests exercise actual runtime upload/copy/delete paths, multiple local
workspaces, conflicts, retry, and zero-payload moves. Real cloud/eventual-consistency
and Windows/WinFsp or Linux/FUSE multi-PC acceptance remain unverified.

## Legacy automatic pool sync v6 — no coordinator (new workspace)

Leave **Automatic history deletion — v7** unchecked for this legacy mode.
Select **Virtual cloud drive → Automatic pool sync** (selected by default for the
virtual GUI workflow). Select the existing pool and a **new persistent workspace
on each PC**. No shared-root field, extra metadata provider, server or designated
coordinator PC is required. An optional worker label identifies conflict copies;
otherwise RPool generates and durably saves a per-workspace `pc-…` label.

RPool discovers the pool's existing encrypted destinations, applies their configured
root paths and replicates immutable metadata under:

```text
<configured-pool-root>/.rpool-sync/events-v6/<pool-name-hash>/events/<event-hash>.json
```

The namespace is stable for the same pool name at the same actual destinations;
remote ordering and adding a destination do not change the hash. **Pool rename
changes namespace identity.** Different PCs must use matching portable pool/remote
configuration pointing to the same encrypted data and keys. Renaming a remote alias
without updating archive references is not a supported migration. Existing workspace
bindings reject changed destination sets/root mappings; topology migration is not
automatic. Never clone an active workspace/device identity across PCs.

Example (the pool and encryption remotes are already configured):

```text
rpool mount --virtual-drive --pool-sync --pool mypool --workspace C:\RPool\pc-a --pool-worker PC-A --mountpoint R:
```

Use another local workspace/worker label on PC B, with the same pool configuration.
Use `--sync-only` instead of a mountpoint to publish/synchronize without mounting.
Different files are independent immutable records; no PC writes a common mutable
catalog. Incoming records are merged by ancestry, not arrival time or PC clocks.
Downloaded metadata also participates in replica repair. Parents publish before
children. Local spool cleanup waits for verified publication to **every configured
metadata destination**; a failed/partial publication retries the same immutable ID.
Any configured metadata destination being unavailable blocks synchronization and
startup, rather than treating an unavailable listing as an empty cloud namespace.

### Conflict list

Same-base concurrent edits preserve the common original at its normal path and
**both** edited heads as `stem_Worker+a-<revision-id>.ext`. The immutable suffix
avoids collisions when two PCs use the same display label. Multi-generation edits
preserve the common fork original and the latest branch heads. Concurrent creation
has no prior original; mixed delete/edit groups explicitly show the delete request.
Ambiguous multiple common bases are displayed as separate original candidates.

The Mount GUI shows **Pool sync · N conflict groups** from structured metadata,
including original paths, worker/device-associated candidates, deletion rows and
**Copy path** buttons. This status is independent of capacity-query success. After
sync exits it is labelled as the last snapshot, not a live cloud view. This release
has no one-click merge/resolution; generic branch edits/deletes must not be confused
with an explicit all-head resolution. All variants remain preserved.

Lists refresh on synchronization. File bytes are downloaded on demand. Generic DAV
clients cannot reliably identify a native editor/read session: after a path has
been read, a revision change fences further opens of that path until **remount**,
rather than mixing old and new range bytes. Existing handles keep their immutable
revision. On restart, previous native VFS cache is isolated under
`recovered-native-cache/` without deletion; inspect it for unsaved work. Native
Windows/WinFsp and Linux/FUSE behavior remains unverified.

### Incremental immutable uploads (Option C)

Automatic pool sync can reuse unchanged remote data from the edit's captured
parent revision. It does not choose another worker's latest revision as the base
and does not merge concurrent edits. The original-plus-worker-branches conflict
policy remains unchanged.

- Plain archives reuse individual unchanged shards; Reed–Solomon archives reuse
  complete unchanged data/parity groups. A changed group is uploaded under fresh
  object identities; borrowed objects are never overwritten.
- Initial optimization requires equal file size, shard size and coding layout.
  Other edits use the existing full uploader. Resilient placement uses the full
  uploader in this initial implementation.
- Reuse initially requires a parent produced by this workspace's durable local
  upload record, with a matching archive identity. Imported archives, metadata-only
  renamed versions, and a first edit of another PC's revision use a full upload,
  because their retained-object lifetime is not established by that upload record.
- Reused objects are verified. A durable per-edit recipe and final manifest keep
  retries on the same source/base and publication bytes. Failures retain local
  pending data rather than acknowledge a broken revision.
- Reading still follows explicit immutable shard references. This is **not**
  byte-range automatic merging, a central metadata service, atomic global CAS,
  or a new remote-durable application `fsync` guarantee.

This reduces repeated payload uploads, not retained revision count. Full integrity
verification still reads remote objects, so this is not a claim of lower total
network traffic or end-to-end latency. Changed-group
archives also have small manifests. Historical objects and metadata remain
append-only; shared garbage collection remains disabled. Composed manifests are
not registered as exclusively owned archives, because they reference earlier
archives. Never manually delete those earlier objects while a revision uses them.
Provider drain with `--delete-source` is rejected for virtual-drive archives;
copy-only migration and reprocessing retain the referenced originals.

### Future history-count configuration — stored, NOT enforced yet

Each workspace has `pool-sync-config.json`:

```json
{ "history_limit": 10 }
```

`history_limit` means desired **previous versions per file**, in addition to the
latest version: 0 means latest-only; 10 means latest + up to 10 previous versions.
Unresolved conflict originals/branches are separately protected. Accepted range
is 0–10000; default 0. This is a local saved setting, not a globally agreed policy.
For eventual peer GC, policy agreement and ownership proofs still need implementation.

Set it in the GUI's **Save future history limit in workspace config**, or with
`--pool-history-limit=10`. Without that override, existing saved config is retained.

**This stage implements automatic metadata management, parallel event publication
and conflict visibility, not peer retention/compaction. The setting does not delete
old cloud versions.** Payload history and causal metadata still accumulate; the
former latest-only v5 policy does not apply to this coordinator-free mode. Fresh
bootstrap is limited to 10,000 unseen events / 64 MiB per replica (and 64 MiB unique
combined metadata); exceeding the bound fails safely. Do not regard this release as
solving long-term storage growth. The self-contained snapshot/GC design remains in
[PEER_SYNC_DESIGN.md](PEER_SYNC_DESIGN.md).

No existing v3/v5/replica workspace or legacy cloud history is silently converted.
Validation: **362 default / 373 optional OpenDAL tests passed**, 12 external-tool
tests ignored per configuration; warning-denied default release build passed.
Tests use synthetic storage and local HTTP; no real cloud files were deleted and no
real multi-PC/native-mount acceptance claim is made.

## Legacy bounded shared-v5 — designated coordinator (opt-in)

For long-running shared drives, select **Virtual cloud drive → disable Automatic pool sync → Legacy bounded shared mode** in the GUI. Use a **NEW workspace on every PC**. Designate exactly **one
coordinator PC/workspace** using the checkbox; leave it off on all other PCs.
The default policy keeps **only the latest version per live file**
(`--shared-keep-previous 0`; explicitly select a positive count only for optional history). Offline PCs **never pin
cloud history**. Deleting a file retires all its owned versions, including history.
Unresolved conflict files remain live files until explicitly deleted.

Example coordinator (Windows, paths illustrative):

```text
rpool mount --virtual-drive --bounded-shared --shared-coordinator --shared-keep-previous 0 --pool mypool --workspace C:\RPool\bounded-coordinator --shared-root crypt:teamspace --worker-name desktop --mountpoint R:
```

Other PCs use the same flags/root but omit `--shared-coordinator`, and use their
own new workspace and worker name. CLI `--sync-only` drains the coordinator's
queue present at entry; workers publish requests and report any writes still
waiting for coordinator acknowledgement. Existing managed virtual-v5 history above this selected count is reclaimed on
the next coordinator sync, even without new writes. Legacy/imported archives are
not affected. The coordinator must be running or
periodically synced to accept writes and reclaim history. There is **no automatic
coordinator election/failover**. Never clone an active coordinator workspace or
run two copies; generic rclone storage provides no distributed compare-and-swap.

On connection, the PC must successfully refresh the authoritative cloud checkpoint
before the drive opens. A failed startup sync leaves local work intact and does not
expose a stale mounted namespace; retry when the cloud is reachable. The file list
is refreshed first, and verified file bytes download on demand, not as a full local
replica. While mounted, reconciliation runs at the configured scan interval; this
is eventual synchronization, not a lock or instantaneous cache coherence.

### Responsive online mount and unmount

Normal online pool mounts now fetch the required cloud metadata before opening the
drive, without first draining pending uploads, replicating history, or running GC.
Metadata/account errors still fail closed; this does not permit stale or incomplete
cloud listings. The legacy bounded shared coordinator retains its full bootstrap
because that operation also initializes a new shared root.

Capacity queries and writeback run in a single background worker after native mount
readiness. Until capacity is verified, the drive reports zero **verified additional**
free space; reads can proceed but Explorer may defer new copies. No invented free
capacity is advertised. The first maintenance pass starts immediately after readiness.

Online unmount cancels remote operations and stops the native mount instead of
waiting for a fresh full cloud synchronization. A stop request is also observed during
startup network operations. Spawned storage processes are cancelled/reaped and the
worker is joined; it is not abandoned with an active workspace lock. Local disk
transactions still finish safely, so this is not a hard wall-clock shutdown guarantee.
Pending spool, uncertain upload receipts and native VFS cache are preserved, not
reported as uploaded. Use **Sync without mounting** / `--sync-only` separately to
complete replication. Preserved native cache may require the existing recovery/export
flow; shared-drive restart does not promise automatic replay of stale native writes.
This fast path applies to online virtual mounts, not legacy full-replica mode.

### Storage and offline-PC policy

- Upload and verify a fresh full-file archive, activate the replacement checkpoint,
  then journal and delete exact obsolete owned objects. Readback uncertainty and
  partial GC resume forward; do not delete transition journals manually.
- Each upload attempt is registered before writing. Old incomplete attempts are
  reswept so a paused writer's late objects do not become permanently untracked.
  Their small unfinished ledger records are retained conservatively; they are
  **not a hard bound on metadata under unlimited failed upload attempts**.
- Clients load a complete current checkpoint rather than replay every historical
  event. A retained checkpoint replaces the preceding one, so revision-event
  growth no longer depends on the number of successful edits. The checkpoint and
  listings still have safety size limits (64 MiB); an arbitrarily large live tree
  is not supported. Corrupt/unavailable metadata fails closed.
- Stale offline edits/deletes are not blindly replayed. If the actual base is still
  current, pending work can continue. Otherwise sealed edits are exported to
  `recovered-writes/<intent>.stale.bin` plus a path receipt; stale deletions are
  recorded without deleting current cloud files. Rejected dependency chains are
  isolated together. Concurrent current-epoch edits may create conflict files.
- Publishing a request is **not acknowledgement**. Local spool is released only
  after a checkpoint acknowledges it (or a durable local recovery export exists),
  and after local reader leases close. Unsynced data remains local on quota/network
  failure. Checkpoint acceptance, not local save success, authorizes cleanup.
- New namespace lookups use the current cloud tree. Already-open read handles may
  fail when their version expires; the program never substitutes different bytes
  for that handle. Native application caches do not provide complete revision
  identity, so transparent cross-PC handle coherence is not promised.
- On each bounded native mount restart, any prior `vfs-cache` is **moved intact**
  to `recovered-native-cache/<id>` before mounting a fresh cache. It is not replayed
  against a potentially newer cloud tree. Inspect/recover those plaintext files
  manually before deleting the recovery directory; the spool export button does
  not decode native VFS cache metadata. Recovery directories are not auto-pruned.

**Capacity headroom:** this still writes complete versions, without cross-version
block deduplication. The default latest-only policy retains one current archive,
with old + new overlap during replacement. If one previous version is explicitly
selected, latest + one previous uses roughly twice the current physical
archive size; replacement can temporarily require a **third** version. With 8+2
coding and full groups, a 10 GiB file uses about 12.5 GiB/version: approximately
25 GiB retained, up to 37.5 GiB while replacing, excluding metadata/provider trash.
Latest-only still needs old + new overlap. Quota is not reserved across PCs; retain
replacement headroom. If the policy cannot fit, writes stay local, not silently
pruned below the chosen policy. Provider trash/versioning may delay quota recovery.

### Compatibility and migration boundary

This protocol uses **`shared-root/virtual-v5`**, local binding/namespace version 5.
Old binaries/workspaces cannot join it. Legacy virtual-v3/replica behavior below is
unchanged and **does not gain automatic cleanup by upgrading the binary**. Existing
old cloud archives are not scanned, adopted, or deleted automatically. Imports
before coordinator initialization remain externally owned and are never swept;
copy current files through the new drive to create owned, bounded versions.
After initialization, import new data by copying its bytes through the drive.
Archive references exported elsewhere do not pin bounded-owned data: do not use
this managed history as permanent archival storage. Preserve any needed backups
outside this retention domain.

Validation on 2026-09-28: **336 default / 347 optional OpenDAL tests passed**,
12 external-tool tests ignored in each. Default release build passed with warnings
denied, and latest-only CLI help/defaults were verified. Development tests use temporary local
files and synthetic storage. No user cloud
objects were deleted or migrated. Real multi-PC cloud/WinFsp/FUSE execution remains
unverified; enable on a test namespace before production use.

## Legacy virtual drive (new workspace only)

Select **Virtual cloud drive** in Mount drive, a NEW empty persistent workspace,
Pool, mountpoint, and optionally shared root + worker name. All participating PCs
must use virtual mode and the same shared root; its events live under
`shared-root/virtual-v3`, isolated from older replica catalogs. Existing archives
are imported explicitly from manifests; listing imports does not download content.
Never point this mode at an existing replica or copy/share an active workspace.

The authenticated loopback DAV bridge mounts through rclone, requiring WinFsp on
Windows or FUSE on Linux. On macOS RPool uses `rclone nfsmount` with the built-in
NFS client, so macFUSE is not required (tested with rclone v1.75.1). Its private persisted
endpoint/token must remain unchanged alongside the VFS cache after interruption.
An occupied saved port or live/uncertain old mount lease fails closed.

- Range reads verify whole requested shards (the archive format has no subshard
  hashes). Missing/unavailable data uses the existing parallel RS recovery scheduler
  for only the affected coding group. Clean shard cache is bounded; dirty spool is
  never evicted. Uncached content needs network access.
- PUT completion at the bridge flushes content, recovery receipt and namespace.
  Native application writes can still be in rclone's VFS cache; keep that cache too.
- Shared changes are fetched while mounted. Previously unserved paths update live.
  Served paths retain an immutable mount-session revision; newer remote versions
  appear as `conflict-incoming-ID` copies until remount. This avoids combining bytes
  from different versions across generic DAV range requests. Local acknowledged
  saves/deletes/renames update their visible paths; already-open DAV read handles
  retain their selected revision. DAV mtime is a synthetic revision discriminator,
  not the original source modification time. **No distributed locking or transparent native
  open-handle coherence guarantee.** Generic writes use conservative observed bases,
  so some sequential edits can produce extra conflict copies rather than overwrite.
- File and directory moves checkpoint their local namespace changes together.
  Empty directories remain local-only. Metadata-only deletion is not remote erasure.
- Usage counts known live shared file contents plus sealed pending changes,
  **excluding parity and cache copies**. A separate committed logical counter is
  shown. Unimported archives and writes still in VFS cache are not included.
- Virtual mode exports this logical usage and placement-aware ceiling via standard
  DAV quota. Real HTTP quota tests pass; Explorer/WinFsp and live rclone mount
  behavior have **not** been validated on a real multi-PC deployment.

### Recovery and safe cleanup

Restart the SAME workspace/mountpoint to replay durable pending work. Do not delete
`vfs-cache`, `spool`, `dav-identity.json`, or mount identity files. If the primary
namespace is corrupt, RPool fails closed instead of silently reverting to a backup
that might omit acknowledged writes.

**Export recoverable spool (unmounted)** / `--virtual-drive --recover-spool` copies
sealed or partial spool files to `recovered-writes`, with receipts preserving the
original paths. It does not repair/replace a corrupt checkpoint or upload anything.
Partial files are explicitly labelled and need inspection. Recovery cannot export
writes that exist only in rclone's cache; recover those through the original mount.

**Trim clean cache** / `--virtual-drive --cleanup-cache` trims verified clean
shards and reclaims verified, committed local spool that has no readers. Pending,
partial, corrupt and unrecognized writes remain available for recovery. Normal
successful sync also reclaims committed spool, but shared writes wait for verified
metadata publication. Read handles and in-progress writers hold leases; cleanup
advances the recovery checkpoint before removing any file.

Top-level DAV spool `content` writes (including local moves) are limited by
`--spool-gib` (default **64 GiB**). V7 captured originals under each intent's
`captured/` directory are currently outside that accounting, so this is **not**
a hard bound on total spool usage. Existing bytes are preserved when the limit
rejects a content write.
This is not a reservation of free disk: VFS cache, staging snapshots, recovery
exports and other applications need additional space. `--cache-gib` remains the
separate clean-shard cache limit (default 10 GiB). CLI status JSON exposes
`spool_bytes`, `spool_limit_bytes` and `pending_writes`.

### Explicit cloud retention — unshared virtual workspaces only

For **unshared** virtual workspaces, automatic cloud deletion is not enabled.
Start with a read-only preview:

```text
rpool mount --virtual-drive --pool mypool --workspace /absolute/workspace --retention-report --keep-previous 3
```

For an **unmounted, drained, exclusively owned** virtual workspace, an operator
can explicitly apply that policy:

```text
rpool mount --virtual-drive --pool mypool --workspace /absolute/workspace --apply-retention --keep-previous 3 --exclusive-archive-ownership
```

The ownership acknowledgment means **no other workspace, PC, exported manifest or
external reader depends on these archives**. Never use it merely because a drive
is currently disconnected. Shared roots are rejected. Pending writes, active
readers, detached VFS cache and unknown/partial spool block the operation; replay
or recover them rather than deleting cache to force maintenance.

- All live versions and unresolved conflict versions are protected, plus the
  requested number of previous tracked versions per original path, in durable
  local upload order. `0` keeps live/conflict versions only.
- Ownership is recorded for successful uploads made by this version. Imported,
  legacy/untracked archives and failed-upload leftovers are **not** automatically
  adopted or deleted. An imported alias of an archive protects its entire identity.
  Thus the first preview may show no reclaimable data despite old cloud usage.
- Retained versions are verified before deletion. Only exact recorded shard and
  manifest objects are removed—never a remote prefix/directory purge.
- A checksummed journal makes interruptions resumable. Ordinary mounting/sync is
  blocked until the same apply command completes; changed `--keep-previous` values
  do not replace an in-progress plan. Do not remove the journal.
- Before deletion, local namespace version **4** fences older binaries. Do not
  downgrade this workspace. A completed sole-writer checkpoint removes obsolete
  local ancestry while keeping every visible/conflict file. Kept historical
  manifests remain in `owned-archives.json`.
- Provider trash, provider-native versions and encryption overhead mean estimated
  logical object bytes are **not proof of immediately recovered provider quota**.
  RPool does not empty provider trash.

**Remaining shared-mode limit:** bounded automatic cloud retention needs an
explicit epoch/checkpoint and offline-writer fencing protocol. Existing shared
roots retain cloud history; this update does not make their storage growth bounded.
Do not manually delete causal events or old archive objects to work around quota.

## Replica GUI

1. Create/select a Pool under Storage → Pools.
2. Open Storage → Mount drive. Select the Pool and a durable local workspace
   (empty folder on first use; choose the same folder on subsequent runs).
3. Optionally select explicit existing manifests to restore. Existing archive Pool
   membership cannot be inferred, so archives are never imported automatically.
4. Choose an unused drive letter such as `R:` on Windows, or an existing empty
   absolute mount directory **outside the entire workspace** on Unix.
5. Mount read/write. Add/edit/rename/delete files through the drive. Close open files,
   then use Unmount and inspect the final scan. Sync once archives already-materialized
   local files without mounting; it cannot drain a detached VFS cache.

Install rclone plus **WinFsp** on Windows, a FUSE-capable rclone on Linux, or
an rclone with `nfsmount` on macOS (tested with v1.75.1; no macFUSE required).
RPool does not install system drivers. A running child
process alone is not considered a ready filesystem. Startup times out after 30 seconds.

## Quota-aware placement and capacity

Mount writeback queries actual backend quotas instead of rejecting provider types
with a fixed allowlist. Missing total/free, contradictory results, unsupported
wrapper resolution and transient query failures have separate diagnostics. Zero
free space means full, not unknown. Exclusion affects new placement only; old
manifests remain readable.

The **Account capacity / outage identities** editor accepts one line per concrete
backing remote (not its crypt wrapper):

```text
backing-a account-a provider-a
backing-b account-b provider-b
```

These are non-secret user declarations, not automatically proven account identities.
Use the SAME quota-group ID for remotes sharing an account/quota. Use the SAME outage
ID for accounts that fail together. Independent budgets are summed; alias budgets
are counted once. Without declarations, all unresolved accounts share the smallest
reported budget conservatively. When some accounts are declared, unresolved accounts
are excluded until mapped, because they might alias one of the declared accounts.
CLI equivalents: `--capacity-domain backing-a=account-a` and
`--failure-domain backing-a=provider-a`.

Capacity uses the SAME round-robin/free-ratio/resilient allocation logic as fresh
uploads, debiting actual account budgets. Resilient now requires declared outage
groups and limits each group's shard concentration to M. It skips targets without
enough account quota, charges shared account budgets once and places larger shards
first within each group. Both upload and estimation use the same allocator;
Resilient metadata replicas go only to targets actually used by that archive.
This is a deterministic feasible greedy policy, not an optimal packing solver.
Saved uploads validate
remaining shards against their saved destinations, crediting only reverified data;
parity remains fully charged. Existing Pool policies are not silently changed.

The panel shows logical used bytes, additional full-group capacity, their sum and
snapshot age. Simulation is bounded to 65,536 physical shards; if capped, the panel
explicitly marks a verified **lower bound, not a maximum**. Small nonempty files need
`L + ceil(ceil(L / shard_size) / K) * M * shard_size` physical bytes. Empty mount files
are uncoded. Metadata/encryption overhead and external writers can reduce usable
space; these are estimates, not reservations or nominal provider capacities.

Replica mode counts local files and OS space remains the local staging disk.
Virtual mode counts its known shared namespace and provides quota through DAV.
Historical archives consume provider quota even though they are not logical live
file usage. No mode infers a complete cloud inventory from a Pool name.

### Move active data away from excluded storage

The panel warns when locally known manifests reference excluded storage. After
unmounting and draining the original VFS cache, **Migrate active archives — retain
originals** restores each affected active archive, uploads it to currently eligible
targets, fully verifies it and atomically switches the workspace reference. Pending
local edits are not substituted for the archived revision and are not overwritten.
The operation holds the workspace lock. Restart reuses a durable verified-copy
receipt only after checking identity, eligibility and remote content again.

Original shards, old manifests and immutable shared history are **not deleted**;
other PCs and historical revisions can still reference them. This action does not
free old storage and does not migrate every historical/cloud object. Warnings count
locally known history as well as active files; unrelated archives elsewhere cannot
be discovered from a Pool name. Manifest-only changes publish shared successor events
so peers receive the new location without losing older history. Shared namespace-root
metadata is separate from shard eligibility and is not relocated by this action.
Partial batch failure preserves completed reference switches; the next shared sync
publishes any pending revisions.

Interrupted uploads reuse verified completed manifests and finish remote metadata
publication. Partially uploaded source-matching data earns quota credit only after
full remote verification; parity is conservatively charged again. Changed eligible
sets use separate upload journals and may leave additional retained copies.

CLI maintenance (no mountpoint required):

```text
rpool mount --pool mypool --workspace /absolute/workspace --capacity-only
rpool mount --pool mypool --workspace /absolute/workspace --migrate-excluded
```

Shared workspaces still require the same `--shared-root` and `--worker-name`.
`--status-file PATH` writes a GUI-readable snapshot atomically; its parent must exist.

## Remembered pool options and configuration changes

Selecting **Upload pool** restores that pool's saved mount options on this PC:
workspace, mount point, automatic pool sync/history deletion and history limit,
worker/shared options, imports, interval and cache budgets. **Save mount settings**
or launching a mount/sync/maintenance action saves them. Switching pools keeps
unsaved drafts in memory, but restart restores only saved profiles. Old settings
without profiles retain the global cache defaults; a new pool never inherits
another pool's workspace or deletion policy.

For an existing online workspace with unchanged storage membership, worker/retry
and supported layout changes use the current pool policy on the next mount instead
of silently using old settings. Existing archive manifests remain unchanged.
An incompatible unfinished upload/layout plan is retained and may require the
transition below instead of an ordinary mount.

After adding/removing storage, use **Apply changed pool to this workspace → Apply
pool changes — keep workspace path** while unmounted. It stages independently
verified current files, visible conflicts and sealed pending writes in a fresh
metadata generation. The public pool name, selected workspace path, and v6/v7 mode
stay the same. Activation happens only after the copied view is published; original
workspace data is retained in a named sibling backup. Interrupted transitions use
the same button/settings to resume; normal opening is blocked until resolved.

This is a one-time migration, not a workspace reset or a metadata-guard bypass.
It can need transfer time and additional disk/cloud capacity. A completed Reprocess
plan can provide verified replacement bytes when an old account is gone; the new
generation still owns independent payloads. Unrecoverable current files prevent
activation. No original cloud objects are deleted by this operation.

**Scope:** old historical versions and dirty native-cache recovery data remain in
the backup, not in the active file view. This does not guarantee old history remains
downloadable from unavailable accounts. Unseen remote changes are not imported.
Old PCs remain isolated on their old metadata generation; this operation does not
automatically migrate an entire multi-PC pool. Do not treat independent transitions
on different PCs as one shared generation.

CLI: `rpool mount --pool NAME --workspace PATH --virtual-drive --pool-sync
--apply-pool-changes` (one line). Keep `--pool-retention` for a v7 source; optional
`--recovery-reprocess-plan PATH` supplies completed Reprocess results. The operation
does not mount automatically; afterward mount the same workspace normally.

## CLI (PowerShell example)

```powershell
rpool mount --pool mypool --workspace C:\RPoolWorkspace --mountpoint R: --interval-seconds 30 --stop-file C:\RPoolControl\stop.request
# Optional imports: append --manifest C:\Manifests\example.rpool.json
# To request shutdown from a second terminal (parent folder must exist):
New-Item C:\RPoolControl\stop.request -ItemType File
# Remove the old stop request before starting again.
rpool mount --pool mypool --workspace C:\RPoolWorkspace --sync-only
```

Keep the service running while using the drive. Force termination is a recovery path,
not a flush guarantee. Restart using the same workspace, source, mountpoint and VFS
cache settings so rclone can recover cached writes. Never delete the cache to clear an
error. If a surviving mount is detected, stop that mount before reopening the workspace.

The first mount saves its source/cache/target identity. Reuse that identity on restart;
moving the workspace or selecting another drive letter is intentionally rejected while
pending cache may exist. An uncertain launch or live/reused PID blocks restart rather
than killing an unrelated process. If the error names `.rpool/mount-process.json`,
first confirm in the operating system that no rclone process or filesystem mount uses
this workspace. Only then may that **lease file alone** be removed for recovery; keep
`mount-identity.json`, all local files and the VFS cache. A clean stop clears the lease
automatically. Ctrl+C/forced termination is not a verified graceful shutdown path.

## Shared workspaces (multiple computers)

Set **Shared encrypted folder** and **Worker name** on the Mount drive screen,
or use both CLI options:

```powershell
rpool mount --pool mypool --workspace C:\RPoolAlice --mountpoint R: --shared-root crypt:teamspace --worker-name Alice
rpool mount --pool mypool --workspace C:\RPoolBob --sync-only --shared-root crypt:teamspace --worker-name Bob
```

Every computer uses its **own local workspace/cache**, but the same actual shared
encrypted directory. Each computer must have working rclone crypt configuration,
keys and remote aliases for the archives being shared. A matching Pool name alone
does not establish a shared folder. Keep worker names recognizable; equal names
are supported through distinct device/revision identities. Never copy an active
workspace or synchronize its `.rpool`/VFS cache between computers.

Shared mode publishes immutable, verified, content-addressed revision records.
The records contain file paths, worker/device identity, parent revisions and full
archive manifests; they are encrypted using the normal crypt-only write policy.
There is no mutable shared catalog whose last uploader wins. Each client retains
all revisions it has observed and computes the same shared file list from them.
Provider listing visibility can delay convergence; a failed listing is not a deletion.

- Independent file edits can be published concurrently.
- Concurrent versions of one file are both retained. One deterministic version
  keeps the original path and the others get worker-named conflict paths with
  revision identifiers. This is not an automatic document merge.
- Concurrent deletion/edit retains the edited version as a named conflict copy.
  Sequential deletion is a logical shared deletion, never cloud archive erasure.
- Offline edits retain the revision that was actually present locally, not a
  newer revision merely discovered while synchronizing.
- Names, case/prefix collisions, malformed records and missing ancestors are
  checked before applying the shared tree. Unsupported combinations fail closed.

**Incoming-file safety boundary:** while a drive is mounted, RPool archives local
changes and exchanges the shared revision list, but does not replace local files
with incoming versions. Incoming additions, updates and deletions are applied at
safe unmounted sync/next startup, only when no mount lease and no persistent VFS
cache files remain. Shared mounts ask rclone to expire clean unused cache entries
on a short (5-second) polling cycle. Close applications and allow writeback/cache
expiry before unmounting. If cache remains, resume
the original mount and let rclone drain/expire it; never delete the cache to force
sync. Shared mode is eventual synchronization, not live network filesystem locking.

Displaced local bytes remain in `.rpool/shared-recovery`. An interrupted apply uses
a local journal and restores displaced bytes without overwriting newer files;
recovery may expose additional `recovered-*.bin` files for manual inspection.
Keep these backups until the shared result is checked. Do not modify the workspace
directly while an unmounted reconciliation runs.

Shared mode currently synchronizes files and their necessary parent directories;
empty directories are local-only. Renames are a new path plus deletion of the old
path. Revision history, tombstones and recovery backups are not garbage-collected.
The shared metadata directory is a required availability dependency (not sharded
across Pool providers). Existing archive data remains independently restorable.
Exchange lists validated hash-prefix pages and charges only **unseen** events
against its per-sync download budget (10,000 events / 64 MiB; 8 MiB per event).
Already-known history no longer triggers the old aggregate hard stop, and replica
sync also uses incremental fetching. Each prefix listing remains bounded by the
transport output limit. A new/long-offline client exceeding the unseen budget
still fails closed; this is **not** a scalable checkpoint/bootstrap protocol or
semantic history compaction. Local shared metadata still grows with history.
Shared workspaces upgrade their local catalog to v2; older RPool binaries refuse
them instead of silently dropping synchronization ancestry. Local-only catalogs
remain v1.

## Transfer scheduling and resilient placement

Mount writeback and shared-file downloads use the archive transfer paths of
put/get. Erasure-coded incoming files automatically use delayed hedged reads and
early group recovery; the safe unmounted-apply boundary remains unchanged. To enable strict shard distribution for a **new** workspace, select a
Pool configured with `Resilient (parity-bound)` and nonzero parity. Existing
workspaces freeze their original Pool policy; editing the Pool alone does not
change them. Empty files retain the existing no-parity handling. See README's
"Strict placement and fair transfers" for bounds and compatibility. This does not
change the deferred incoming-apply rule or turn the mount into cloud read-through.

For virtual WebDAV mounts, rclone waits for 60 seconds of no access after a
file closes before writing its VFS cache back to RPool. This reduces repeated
small NFS writes becoming separate full-file revisions, but it is not a hard
amplification bound: allow local VFS-cache and spool space, and check pending
writeback before assuming cloud durability. On macOS, a native NFS shutdown
that exceeds the grace period leaves rclone running and retains the mount lease
instead of killing the server during kernel I/O. Inspect the OS state before
manually recovering such a workspace.

## Local workspace durability and limits

- Keep **the entire workspace**, including `.rpool`, `files` and `vfs-cache`, on reliable
  local storage. These contain plaintext; use OS disk encryption as appropriate.
- Allow space for imported files, VFS cache, and temporary immutable upload snapshots.
  Initial imports fully download and verify selected files before exposing them.
- Each changed file creates a fresh archive version. Interrupted uploads retain their
  snapshot/identity for retry; the previously committed archive is not overwritten.
- Deletion creates a local catalog tombstone, **not remote erasure**. Prior cloud
  versions remain in inventory and continue consuming capacity. Rename currently
  archives the new path and tombstones the old one.
- In local-only mode, directory structure/empty directories and logical deletion state live in the local
  catalog. This mode is not a distributed multi-machine namespace; back up the workspace
  metadata. Individual cloud archive manifests cannot reconstruct the complete tree.
- A workspace freezes its Pool policy on creation. Later Pool configuration changes
  do not silently redistribute it. Use a new workspace to select a different policy.
- One writer owns each local workspace. Separate workspaces participate in shared
  conflict handling only when explicitly bound to the same shared encrypted folder.
  Do not edit internal metadata/transactions/cache.
- Symlinks, junctions, special files, case-colliding names and nonportable Windows names
  are rejected. Imports need a hard-link-capable filesystem (for example NTFS/APFS).
  This is not a sandbox against a hostile local process swapping filesystem paths.
- Shutdown reports pending scan changes and local VFS state when available. Open or
  cached writes may remain even after a successful final scan. Force stop retains
  cache but cannot guarantee recovery of data an application never saved.

Validation uses isolated temporary files and fake upload failures. Actual Windows
Explorer/WinFsp, macOS/Linux FUSE and live cloud mounting remain unverified here.

2026-09-28 hardening validation (macOS): **306 default tests / 317 optional
OpenDAL tests passed**, 12 external-tool tests ignored in each suite. Default
release build passed with warnings denied; release `mount --help` flags verified.
Synthetic tests cover retention crash-replay boundaries, live-reader protection,
spool budget enforcement, shared-publication protection and Resilient quota/
metadata placement. No real cloud deletion or Windows/Linux native mount test
was executed. These figures describe the earlier unshared hardening batch. The bounded
virtual-v5 protocol above adds shared checkpoints/retention; legacy oversized
cold bootstrap remains unchanged.

References: [rclone mount](https://rclone.org/commands/rclone_mount/),
[local backend](https://rclone.org/local/).


## Local cache size and least-recently-used eviction

In **Storage → Mount**, **Online drive** is now selected by default for metadata-first
browsing and on-demand downloads. It uses the existing virtual filesystem and
automatic pool sync, without retaining a full local copy. All files in the
synchronized/imported namespace stay visible; this does not discover unrelated
objects in a provider account. A replica workspace retains a full plaintext copy; its
`files/` directory is not a disposable cache. Use a new virtual workspace rather
than changing/deleting an existing replica's files.

The mount form exposes independent limits (GiB = 1024³ bytes):

| Setting | CLI | Default | Behavior |
| --- | --- | --- | --- |
| Clean shard cache budget | `--cache-gib` | 10 GiB | Verified immutable shards, oldest successful access evicted first before download admission |
| Native OS cache target | `--vfs-cache-gib` | 10 GiB | rclone's independent cache; clean unused data evicted under pressure |
| Keep disk free | `--cache-min-free-gib` | 2 GiB | rclone native-cache free-space target, **not** a reservation |
| Pending write spool limit | `--spool-gib` | 64 GiB | Virtual pending writes; rejects growth instead of discarding unsynced data |

For example, add `--cache-gib=4 --vfs-cache-gib=4 --cache-min-free-gib=2 --spool-gib=8` to a virtual mount command. Settings apply when the next process
starts. **Save cache settings** persists these limits and the online/replica
selection on this PC. **Mount read/write** and **Sync without mounting** also
save them before starting. Restarting the app restores them. Workspace paths and
cloud history settings are not changed by saving cache preferences. Supply CLI
overrides on each launch; CLI compatibility is unchanged and online mounts still
require `--virtual-drive --pool-sync` explicitly.

Example: 4 GiB shard + 4 GiB native cache means 8 GiB of clean-cache targets,
with an additional 8 GiB pending-write spool if configured as above. Space is
not preallocated. Verified, published write spool is automatically released when
no live reader needs it. Pending or uncertain writes are retained.

### Switching from a full replica

Create a **new empty persistent workspace**, select Online drive and the pool,
then mount it. Existing online peers with the same pool mapping synchronize their
namespace. Legacy replica files are **not automatically migrated**: first finish
their original writeback, then explicitly import the desired verified archive
manifests into the new online workspace. Imports use archive filenames, not the
original replica folder tree; recreate desired folders/names and inspect any
same-name conflict copies. Optional v7 history mode copies imports into independent
snapshots and needs additional local working space. Validate contents before manually
retiring any old replica. Do not delete `files/` or VFS recovery data as a cache
cleanup operation. Keep using the original replica workspace to recover pending
writes. Switching the checkbox does not convert a workspace in place.

Reading evicted content downloads and verifies it again; offline access to it is
not available. Shard download/recovery admission refuses a working set that does
not fit the shard budget: increase that budget when the error requests it. `--cache-gib=0` disables
nonempty shard download admission; it does not mean unlimited storage.

**These are not one aggregate hard disk limit.** The shard and native caches are
separate copies, and the write spool adds to both. Native eviction is polled every
five seconds; open files and pending writes cannot be safely evicted and can
exceed its target. Shared mounts also expire unused native cache promptly for
safe reconciliation. Upload staging, metadata, recovered native caches, exported
recovery files and other applications need additional disk space. No automatic
cleanup deletes dirty/unknown data, recovery directories, full replica files or
remote cloud objects.

Native limit semantics: [rclone mount VFS cache documentation](https://rclone.org/commands/rclone_mount/#vfs-file-caching).
