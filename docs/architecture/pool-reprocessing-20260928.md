# Storage UI and copy-only pool reprocessing

## User workflow

1. Settings defines the defaults for a new pool. In Storage → Pools use New / clear to copy the current settings; loading an existing pool preserves its saved policy.
2. Choose encrypted destinations explicitly. Discovered providers are not automatically added. Settings' saved selected destinations are inherited for new pools, not every discovered provider.
3. Save the changed pool. This changes future uploads only; existing manifests remain valid and readable.
4. Open Storage → Reprocess data, choose the saved target pool and explicitly select archived manifests (local library, local files or remote manifest paths).
5. Calculate / preview. The worker reads and validates manifests, freezes the destination roots/policy, and saves a plan locally. No archive data is written remotely during preview.
6. Review logical data, additional remote storage, total download/upload traffic and approximate duration. Create reprocessed copies starts a background task. Completed replacements appear as new library entries; originals stay intact.

The application cannot prove pool ownership from old manifests: neither manifest nor inventory stores a pool ID. A shared provider does not imply pool ownership. Thus no automatic selection of all supposedly affected archives occurs.

## Storage layout and time estimate

K/M, shard size and destination changes require restoring plaintext and creating a new archive. Worker/retry defaults alone do not invalidate stored data and do not require rewriting it; the explicit copy action still performs a full rewrite when requested.

For logical bytes B and new physical shard bytes T, approximate traffic is upload T, download B + 2T (restoration, write readback and a separate full verification). Final partial groups include full-size parity shards. ETA is based on user-entered aggregate download/upload MiB/s, not guessed provider performance or worker count. With no rates ETA is unknown. CPU/disk/encryption overhead, metadata, throttling, retries and degraded recovery are not covered; this is not a completion-time guarantee.

Original archives are retained, so T is additional remote space. Local scratch needs at least approximately twice the largest restored file plus parity/transfer spools. Free capacity is not reserved or guaranteed by this estimate.

## Safety and limits

Plans freeze source manifests/fingerprints and target policy, are checksummed, and are rechecked before execution. New random archive namespaces isolate conversion from originals. Prepared receipts are written before uploads; verified local manifests are persistent before inventory registration. Old objects, manifests and inventory entries are never deleted by reprocessing.

Interrupted attempts can leave new partial objects; receipts identify their namespaces. Re-executing a plan creates fresh copies and may duplicate already-completed replacements; it is not transactional in-place conversion or automatic cleanup/resume. Cancellation does not authorize deletion of any original. Do not run conversion again merely because a response was lost; inspect receipts and task logs first.

## CLI

```sh
rpool pool plan-reprocess my-pool --manifest existing.rpool.json --download-mib-s 20 --upload-mib-s 10
rpool pool reprocess --plan /path/printed/by/planning/plan.json
```

## Layout

Storage Pools has independent destination and policy scroll panes. Providers separates health controls from migration controls. Reprocess separates selection/target configuration from calculation results. No whole-page scroll encloses these panes. Dashboard Health is adjacent to Reported capacity, with Files below.

## Verification

macOS ARM64 validation for source snapshot v0.5.15-r5:

- Default suite: 159 passed, 0 failed; 11 external-tool tests ignored.
- OpenDAL optional suite: 170 passed, 0 failed; 11 external-tool tests ignored.
- Default release build succeeded; no compiler warnings in these runs.
- Headless egui 800×600 test verifies bounded adjacent Storage panes and mouse-wheel isolation.
- Synthetic backend conversion tests verify plain → RS → plain byte equality, exact original filename metadata, retained original shards/replicas and no published success manifest after injected upload failure.
- Independent review found and corrected a missing per-remote root resolution step before plan freezing.
- Both new CLI help paths execute successfully.

Windows interactive GUI and live-cloud conversion remain unverified. No user cloud data was modified during development. Initial-setup files are unchanged.
