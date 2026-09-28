# Storage UI and copy-only pool reprocessing

## User workflow

1. Settings defines the defaults for a new pool. In Storage → Pools use New / clear to copy the current settings; loading an existing pool preserves its saved policy.
2. Choose encrypted destinations explicitly. Discovered providers are not automatically added. Settings' saved selected destinations are inherited for new pools, not every discovered provider.
3. Open Storage → Reprocess data and choose a saved pool as the starting point. Edit an independent target draft: add/remove encrypted providers and change shard size, K/M, workers, retries or placement. Editing the draft does not change the saved pool or stored data.
4. Explicitly select archived manifests (local library, local files or remote manifest paths). The separate **Save draft as pool defaults** action updates future uploads only; it does not perform conversion.
5. Calculate / preview. The worker reads and validates manifests, freezes the destination roots/policy, and saves a plan locally. No archive data is written remotely during preview.
6. Review destination/layout changes, logical data, additional remote storage, total download/upload traffic and approximate time until selected replacements are verified and indexed. Create reprocessed copies starts a background task. Completed replacements appear as new library entries; originals stay intact.
7. After cancellation, use **Resume saved operation**. After restarting the application, use **Open saved plan…** to select that operation's plan.json and review its frozen policy and sources before resuming. A saved plan is independent of the current editable draft.

The application cannot prove pool ownership from old manifests: neither manifest nor inventory stores a pool ID. A shared provider does not imply pool ownership. Thus no automatic selection of all supposedly affected archives occurs.

## Storage layout and time estimate

K/M, shard size and destination changes require restoring plaintext and creating a new archive. Worker/retry defaults alone do not invalidate stored data and do not require rewriting it; the explicit copy action still performs a full rewrite when requested.

For logical bytes B and new physical shard bytes T, approximate traffic is upload T, download B + 2T (restoration, write readback and a separate full verification). Final partial groups include full-size parity shards. ETA is based on user-entered aggregate download/upload MiB/s, not guessed provider performance or worker count. With no rates ETA is unknown. CPU/disk/encryption overhead, metadata, throttling, retries and degraded recovery are not covered; this is not a completion-time guarantee.

Original archives are retained, so T is additional remote space. Local scratch needs at least approximately twice the largest restored file plus parity/transfer spools. Free capacity is not reserved or guaranteed by this estimate.

## Safety and limits

Plans freeze source manifests/fingerprints and target policy, are checksummed, and are rechecked before execution. New random archive namespaces isolate conversion from originals. Prepared receipts are written before uploads; verified local manifests are persistent before inventory registration. Old objects, manifests and inventory entries are never deleted by reprocessing.

New plans write atomic, fsynced completion checkpoints after verification and inventory registration. Resuming rechecks completed replacements in full, repairs their inventory registration if necessary, and avoids copying them again. An unfinished item restarts under a fresh archive ID, never over the original. Interrupted partial objects and receipts can remain; no automatic deletion occurs. A per-plan OS lock prevents concurrent execution and releases on process exit/crash. Inventory writes replace the index atomically without first deleting the old index; concurrent add operations are serialized.

Keep original providers connected until all selected replacements are verified and indexed. Removing a destination from the target does not remove its rclone configuration or delete its original data; unselected archives may still depend on it. A pool-wide readiness guarantee is impossible without explicitly selecting every affected archive.

The initial time estimate is for a full-copy operation, not minimum-shard movement. Resume time differs: completed copies need verification reads, while unfinished files restart. Version 1 plans still load, but old successful attempts without completion checkpoints can be copied again. Corrupt checkpoints or changed source manifests stop safely instead of silently declaring success. Existing crypt/cloud data is not protected against independent account deletion or external modification by this workflow.

## CLI

```sh
rpool pool plan-reprocess my-pool --manifest existing.rpool.json --download-mib-s 20 --upload-mib-s 10
rpool pool reprocess --plan /path/printed/by/planning/plan.json
```

## Layout

Storage Pools has independent destination and policy scroll panes. Providers separates health controls from migration controls. Reprocess separates selection/target configuration from calculation results. No whole-page scroll encloses these panes. Dashboard Health is adjacent to Reported capacity, with Files below.

## Verification

Current macOS regression run: default 196 passed, optional OpenDAL 207 passed, 12 ignored in each. Release compilation passed with warnings denied. Actual cloud cancellation/resume and Windows/Linux GUI execution remain unverified.

Current regression coverage additionally exercises completion revalidation without new writes, interrupted-item retry with original bytes unchanged, invalid receipt/manifest rejection, unavailable replacement detection, kernel-lock exclusion/release, legacy-plan loading, preview invalidation and atomic inventory replacement. No live cloud operations are performed by these tests.

macOS ARM64 validation for source snapshot v0.5.15-r5:

- Default suite: 159 passed, 0 failed; 11 external-tool tests ignored.
- OpenDAL optional suite: 170 passed, 0 failed; 11 external-tool tests ignored.
- Default release build succeeded; no compiler warnings in these runs.
- Headless egui 800×600 test verifies bounded adjacent Storage panes and mouse-wheel isolation.
- Synthetic backend conversion tests verify plain → RS → plain byte equality, exact original filename metadata, retained original shards/replicas and no published success manifest after injected upload failure.
- Independent review found and corrected a missing per-remote root resolution step before plan freezing.
- Both new CLI help paths execute successfully.

Windows interactive GUI and live-cloud conversion remain unverified. No user cloud data was modified during development. Initial-setup files are unchanged.
