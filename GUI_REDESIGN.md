# rpool GUI redesign plan

The GUI redesign is intentionally split into small patches. Each patch must preserve existing storage behavior and keep CLI/storage logic out of GUI presentation code.

## Design direction

The target is a conventional desktop storage-management application rather than a card-heavy or assistant-like interface.

- compact desktop layout
- tables and toolbars before decorative cards
- restrained status colors
- advanced controls hidden until needed
- storage/domain logic remains outside `gui/`
- raw task output remains available, but not as the primary user experience

## Patch sequence

### Patch 1 — GUI internal structure — implemented in v0.5.5

- split application state into `gui/state/`
- create dedicated navigation ownership
- create central theme/layout ownership
- preserve the current screens and visible layout
- do not change storage behavior

### Patch 2 — Navigation and top-level layout — implemented in v0.5.6

Replace the current feature-by-feature sidebar with:

- Dashboard
- Files
- Storage
- Jobs
- Maintenance
- Settings

Existing screens remain reachable through local section tabs while later patches progressively redesign each work area. Current grouping:

- Files: Inventory, Upload, Restore, Verify, Status
- Storage: Providers, Pools
- Jobs: current operation and history actions
- Maintenance: Integrity, Manifest replicas, System diagnostics

### Patch 3 — Theme and common widgets — implemented in v0.5.7

Centralized spacing, typography primitives, row/control heights, panel widths, restrained borders/radius, and semantic status colors. Added reusable status badges, exact capacity bars, toolbars, and section headers while preserving the active light/dark theme and existing workflows.

### Patch 4 — Dashboard — implemented in v0.5.8

Created a summary-first dashboard for known physical capacity, indexed file count/logical size, provider capacity, pool readiness, warnings, and recent jobs. Detailed operations remain in Storage and Jobs.

### Patch 5 — Files — implemented in v0.5.9

The local inventory is now the main file-management surface with search, pool/erasure-coding filters, sortable columns, file selection, a details pane, and direct Restore / Verify / Status transitions. Live health is intentionally not inferred from inventory metadata; the browser shows `Not checked` until a remote status/verification workflow is run.

### Patch 6 — Upload UX — implemented in v0.5.10

Move normal upload into a simple workflow/dialog with pool selection and drag-and-drop. Keep shard/RS/placement/worker controls under Advanced settings.

Internal steps:

1. **Pool-centric upload foundation — implemented**
   - split upload UI into state, target selection, manual policy, and task-start ownership
   - make storage pools the normal/default destination path
   - retain manual destinations as an explicit one-off path
2. **File list and drag-and-drop input — implemented**
   - support multi-file selection and desktop drag-and-drop
   - keep a visible per-file queue with size and upload state
   - upload queued files sequentially through the existing `rpool put` path
   - pause the batch on failure/cancellation while keeping remaining files pending
3. **Collapsible Advanced settings and manual destination controls — implemented**
   - keep the normal upload surface focused on files + storage pool + upload
   - move manual one-off destinations behind a collapsed Advanced section
   - keep shard/RS/placement/workers/retries visible only for manual one-off uploads
   - move optional custom Archive ID into Advanced and keep multi-file IDs automatic
   - keep pool uploads governed by the selected pool policy instead of exposing duplicate overrides
4. **Upload summary, validation, and final UX cleanup — implemented**
   - show pending files, total bytes, target, coding policy, and encrypted destination count before execution
   - block Start when files, pool/manual policy, RS parameters, or known crypt destinations are invalid
   - keep crypt discovery failures as explicit warnings while retaining authoritative low-level crypt enforcement
   - reuse the same preflight validation in the visible summary and actual batch start path
   - cache loaded pool definitions in GUI state so preflight rendering does not perform filesystem reads every frame

Patch 6 is complete; the v0.5.10 release archive includes all four internal steps.

### Patch 7 — Jobs and progress — in progress for v0.5.13

Separate normal operation progress from raw stdout/stderr. Add running, queued, and historical job views and expose raw logs through expandable details.

Internal steps:

1. **A1 — Unified job-state foundation — implemented**
   - use one `JobStatus` model for pending/running/completed/failed/cancelled states
   - share the status model with the upload queue instead of maintaining a duplicate upload-only enum
   - separate task metadata/progress from raw log lines
   - retain batch position in structured `TaskProgress` for the later progress UI
   - split the former monolithic `task_runner.rs` into `gui/task/model.rs` and `gui/task/runner.rs`
2. **A2 — Progress UI — implemented**
   - use a dedicated structured progress protocol instead of parsing human-readable rclone/rpool log text
   - keep logical completion bytes separate from actual network-transferred bytes so resumed/skipped shards remain accurate
   - show an exact-width progress bar, batch position, bytes, average transfer rate, ETA, and cancel without exposing console output by default
   - keep command/stdout/stderr under a collapsed Details section
   - emit upload and restore progress from the existing storage paths without changing archive or manifest formats
3. **A3 — Jobs screen — implemented**
   - separate Running, Queue, and History views
   - integrate the upload batch queue with the common job-state presentation
   - read history directly from the local history store so it remains visible while other jobs run
   - refresh Jobs history and Dashboard summaries automatically after child operations finish
   - share job status badges and presentation formatting across Jobs, Upload queue, and Dashboard
4. **A4 — Retry and details — implemented**
   - show exact current-session command/output under expandable Details
   - show persisted History failure messages, target, timing, and job ID without inventing missing command arguments
   - retain the exact original GUI invocation in memory and allow Retry only for failed/cancelled operations that still have that invocation
   - route failed upload retries back through the upload batch queue so queue state and subsequent pending files stay consistent

Patch 7 is complete in v0.5.13. The release patch archive contains the cumulative A1–A4 changes plus the upgrade cleanup script.

### Patch 8 — Storage — in progress for v0.5.14

Consolidate provider capacity, provider health, pools, remote roots, drain, and migration into one storage-management area.

Internal steps:

1. **B1 — Provider list — implemented**
   - show physical provider capacity and health in one compact table
   - display exact usage, used/free values, reachability, and latency
   - refresh capacity and health together through the existing background refresh path
   - keep union/crypt/chunk/chunker out of the physical-capacity list
   - retain migration controls without making them the primary provider surface
2. **B2 — Provider details — implemented**
   - add provider selection and a dedicated details panel for physical backing path, linked crypt mappings, capacity, latency, health, and default root
   - resolve crypt-to-physical mappings during the existing background provider refresh rather than blocking the UI thread
   - relaunch Windows GUI invocations as a no-console child process so the native GUI does not keep a terminal window open while CLI invocations remain normal console processes
3. **B3 — Pool management — implemented**
   - use a list/details workflow for coding policy, crypt targets, physical failure domains, and placement
   - estimate raw backing capacity and usable free capacity after Reed-Solomon overhead
   - analyze single-provider failure safety without double-counting crypt remotes that share one physical backing
4. **B4 — Safe drain/migration workflow — implemented**
   - require Preview → Validation → Confirmation → Execution for provider migration
   - run preview through the real backend dry-run path and display the exact shard move plan
   - invalidate preview when any migration input or safety option changes
   - keep risky failure-domain overrides and source deletion explicit and visible before confirmation

Patch 8 is complete in v0.5.14. The cumulative patch ZIP contains B1–B4 plus upgrade cleanup.

### Patch 9 — Maintenance

Consolidate scrub, repair, manifest replica management, inventory rebuild, and doctor/diagnostics.

### Patch 10 — Settings

Group settings into General, Storage, Defaults, and Advanced sections.

### Patch 11 — UI persistence

Persist window geometry, sidebar width, last page, last selected pool, table widths, sorting, filters, and advanced-mode state.

### C3 — Repair workflow — complete

- Reuse the last matching Scrub snapshot as the repair-selection source.
- Separate Recoverable, Unrecoverable, and Provider-error groups.
- Permit selecting only groups proven recoverable by Reed-Solomon availability.
- Always perform a Full BLAKE3 pre-check before reconstruction.
- Support repeated CLI `--group` arguments so the GUI and backend use the same selective-repair path.
- Re-verify selected groups after repair and persist the resulting integrity snapshot.


### Patch 12 — Mount screen — implemented after 0.8.0

The former 1,200-line single-page Mount screen is split into
`gui/screens/storage/mount/`, one file per part. From top to bottom:

- **Status bar.** A Not mounted / Running / Stopping badge, the pool and
  mountpoint, and the primary actions (Mount, Sync now, Check capacity;
  Unmount and Force stop while running).
- **Drive.** Pool, Mode (Online drive / Full local replica), Sync (Automatic
  pool sync / This PC only / Shared root), workspace and mountpoint.
  Explanations moved into tooltips.
- **Capacity.** A verdict that uses the drive's own free-space rule:
  "Writable: N can be written now", or "Not writable yet" with the reasons
  (stale measurement, unanswered accounts, undeclared identities, missing
  outage groups). A per-account table follows, and the notes are collapsed.
- **Account identities.** A table editor (backing remote, capacity group,
  outage group) pre-filled from measured accounts, replacing the free-text
  box.
- **Advanced** (collapsed). Cache/spool/interval, filesystem frontend,
  history, archive imports, maintenance, apply pool changes, and account
  recovery.
- **Pool-sync conflicts, and the log** (open while running).

Form state, per-pool persistence and CLI arguments are unchanged. A headless
egui test draws the screen in every mode and capacity state.

## Rule for this redesign

Do not start the v0.6 storage feature set until the GUI restructuring is stable enough that new backend features have a clear place in the interface.


## v0.5.15 — Maintenance UI integration

### C1 — Integrity overview — complete

- Persist the most recent scrub/repair integrity summary.
- Show healthy, missing, corrupt, recoverable, and unrecoverable state at a glance.
- Show archive, scan mode, degraded groups, and relative last-check time.
- Keep provider/authentication probe errors distinct from corruption.

### Remaining steps

- C2 — Scrub workflow redesign — complete.
- C3 — Repair workflow linked to scrub results — complete.
- C4 — Metadata and diagnostics integration — complete.


### C2 — Scrub workflow — complete

- Select an indexed Library file or enter a manifest path directly.
- Choose Quick existence/size checks or Full BLAKE3 streaming verification.
- Show a pre-run summary before starting a scrub.
- Report shard-count progress through the structured GUI progress channel.
- Persist and display non-healthy shard details from the last scrub/repair result.


### C4 — Metadata / Diagnostics — complete

- Group manifest replica verify/replicate/recover under Metadata.
- Place inventory rebuild alongside manifest maintenance because both are metadata operations.
- Present Doctor results directly as a diagnostics table instead of another raw operation console.
- Include local integrity snapshot readability in Doctor checks.
- Retire the old single-file Manifest/System maintenance screens.
