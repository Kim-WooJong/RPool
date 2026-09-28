# rpool development rules

## Storage pivot validation checkpoint

Run cargo test --locked --bin rpool and cargo test --locked --features opendal-prototype --bin rpool, plus both cargo build variants. The 3-OS workflow includes these configurations but has not been executed here. Retired paths are listed explicitly in scripts/update-cleanup.nu; review with --dry-run before applying to an older tree. No blanket dead-code/unused suppression: preserve justified contract/crypto surfaces and record remaining diagnostics. See docs/architecture/verification.md for executed vs pending checks.

The project follows three structural rules by default.

1. **One logical feature, one owning file.**
   - A CLI command implementation owns one file; command families use a dedicated folder such as `commands/pool/`.
   - A feature-specific CLI schema belongs under `src/cli/<feature>.rs` when the feature has subcommands or meaningful arguments.
   - A standalone GUI screen belongs in `src/gui/screens/<feature>.rs`; related screens belong under an owning work-area folder such as `src/gui/screens/files/`.
   - A reusable GUI control belongs in `src/gui/widgets/<control>.rs`.
   - GUI application state belongs under `src/gui/state/`; keep persisted-state loading separate from the state data structure.
   - Top-level GUI navigation belongs in `src/gui/navigation.rs`.
   - Shared GUI layout/theme constants belong in `src/gui/theme.rs`; screens should not duplicate global layout values.
   - Reusable capacity bars, status badges, section headers, and toolbars belong in `src/gui/widgets/`; screens should compose them instead of repainting equivalents.
   - Multi-step GUI workflows should keep validation in a dedicated feature file and reuse the same validation result for both presentation/preflight and task execution.
   - Reed-Solomon behavior belongs in `src/erasure/`.
   - rclone transport behavior belongs in `src/storage/`.
   - Manifest-specific behavior belongs in `src/manifest/`.
   - Pool, inventory, history, diagnostics, maintenance/repair, provider operations, remote-root configuration, portable config synchronization, and journals each own their respective domain directories.
   - A file may contain small private helper functions that exist only to implement that single feature.

2. **Reusable behavior must not live inside command orchestration.**
   - Generic file, JSON, hashing, path, formatting, or validation logic belongs in a reusable module.
   - If the same behavior would be useful from two features, extract it before duplicating it.
   - Domain-specific reusable code should live with its domain (`storage`, `manifest`, `erasure`, `pool`, etc.), not in a catch-all command file.

3. **Related files are grouped by domain.**
   - Do not grow a new monolithic `main.rs`, `application.rs`, CLI file, or generic `helpers.rs`.
   - Prefer a small module directory with explicit ownership over a large miscellaneous file.

## Metadata invariants

- Manifests are authoritative archive metadata.
- The inventory is a rebuildable cache and must never become required to restore an archive.
- Task history is operational metadata and must not contain credentials, tokens, or complete secret-bearing command lines.
- Manifest replicas should remain independently readable full copies.
- Pool configuration is reusable policy, not archive metadata; an existing manifest must remain usable if a pool is later edited or removed.
- Per-remote default paths live in `remote_roots.json`. They are configuration aliases only: an explicit path in a manifest or CLI target remains authoritative.
- Portable dotfiles synchronization uses the versioned `rpool-portable-config` bundle. Only reusable configuration belongs there; credentials and machine-local operational state must stay out.
- Every remote object written by rpool must target an rclone `crypt` remote with content encryption enabled (`no_data_encryption != true`). Capacity/placement logic resolves virtual crypt/chunker layers to physical backing remotes instead of bypassing encryption for writes.

## Dependency direction

Use this direction unless a feature has a concrete reason not to:

```text
main.rs
  ↓
application.rs / cli/
  ├──────────────→ gui/
  ↓                 ↓
commands/          presentation/
  ↓
pool/ inventory/ history/ doctor/
maintenance/ provider/ remote_root/ config_sync/ journal/
manifest/ planning/ erasure/ storage/
              ↓
            models/
              ↓
            utils/
```

Additional constraints:

- `main.rs` owns binary crate module registration and starts the application entry point; it does not implement feature behavior.
- `application.rs` dispatches CLI commands and cross-cutting task recording; it does not implement storage behavior.
- `commands/` coordinates features but does not duplicate transfer, hashing, quota, pool, inventory, or manifest logic.
- `gui/` owns presentation and GUI orchestration only. Long-running storage operations should reuse existing command behavior rather than reimplementing storage logic.
- `models/` contains serializable/domain data structures and lightweight model-local behavior only.
- `presentation/` formats output and does not perform remote I/O.
- `utils/` contains low-level domain-independent helpers; domain logic should not be moved there merely to shorten another file.

## Adding a feature

Before adding code, decide its owner:

- new CLI action → `commands/<domain>/` or `commands/<feature>.rs`
- new CLI schema family → `cli/<feature>.rs`
- new GUI screen/action → `gui/screens/`
- reusable GUI control → `gui/widgets/`
- new Reed-Solomon/erasure behavior → `erasure/`
- new remote/rclone operation → `storage/`
- new archive/manifest rule → `manifest/`
- reusable pool policy/config behavior → `pool/`
- catalog/index behavior → `inventory/`
- operation-history behavior → `history/`
- maintenance diagnostics → `doctor/`
- integrity scanning / repair orchestration → `maintenance/`
- provider health / migration policy → `provider/`
- provider-specific default/base path policy → `remote_root/`
- portable dotpush/dotpull configuration bundles → `config_sync/`
- resumable-operation state → `journal/`
- new provider-selection rule → `placement/`
- new upload-layout/failure-domain rule → `planning/`
- new output view/table → `presentation/`
- new reusable low-level primitive → `utils/`

If no existing domain fits, create a narrowly named module instead of appending unrelated code to an existing file.

## Verification workflow

For refactors and feature additions:

- verify every `mod` declaration has a matching owning source file;
- inspect delimiter/string balance and obvious syntax structure;
- verify internal imports and command dispatch wiring;
- check that shared behavior has a single owner rather than duplicated copies;
- validate JSON/JSONL metadata formats and backward-compatibility assumptions;
- inspect CLI/README/CHANGELOG consistency;
- do not rely on generated caches as the only metadata copy.

`cargo check` and builds are intentionally not part of this project's requested development workflow unless explicitly requested later.

## GUI navigation structure

The GUI exposes only six top-level work areas: Dashboard, Files, Storage, Jobs, Maintenance, and Settings. Feature-specific navigation belongs inside the owning screen folder rather than in the global sidebar.

## GUI visual rules

- Preserve the active egui light/dark theme; do not introduce gradients, neon accents, glass effects, or decorative card stacks.
- Shared spacing, control heights, corner radius, and semantic colors are owned by `gui/theme.rs`.
- Status color is semantic only: success, warning, error, and neutral. Normal navigation and primary content should remain theme-native.
- Capacity bars must derive their filled pixel width directly from a clamped `used / total` ratio.
- Keep raw operation output available, but do not make console styling the primary GUI hierarchy.

- Files inventory presentation is split under `src/gui/screens/files/inventory/` into data mapping, filters, table, details, rebuild controls, and view state.

## Upgrade hygiene

- Every version that retires or relocates source files must update `scripts/update-cleanup.nu`.
- The cleanup list must be explicit; never delete broad directories or glob arbitrary user files.
- The script must verify it is operating on an rpool project before deleting anything.
- Keep `--dry-run` available so an in-place update can be inspected before removal.
- A clean extraction should never require the cleanup script; it exists for safe overlay upgrades.


## GUI progress protocol

GUI-launched child operations set `RPOOL_PROGRESS_PROTOCOL=1`. Core upload/restore paths may emit `@rpool-progress` JSON events to stderr through `src/progress/`. The GUI task runner consumes these events before raw-log capture, so progress rendering must not depend on human-readable rclone/rpool log text. CLI runs without the environment variable remain unchanged.

## Job retry safety

- GUI retry metadata is session-local: persisted task history must not be treated as an executable command source unless a future format explicitly stores a validated retry plan.

## Provider migration safety

- GUI provider migration must follow Preview → Validation → Confirmation → Execution.
- Preview must invoke the same backend dry-run validation path as execution rather than duplicating storage-policy checks in presentation code.
- Any change to manifest, source, destination, output, deletion policy, or risky-safety override invalidates the previous preview.
- Source deletion remains opt-in and must occur only after destination copy, full verification, manifest replacement, and replica update succeed.


### Maintenance integrity state

The latest scrub/repair summary is stored as local derived metadata (`integrity.json`). It is not a source of truth for archive recovery and may be regenerated by running scrub again. GUI maintenance views must not infer healthy state when no integrity snapshot exists.

### Selective repair safety

- GUI repair selection is derived from the most recent integrity snapshot for the exact selected manifest.
- Only groups with known missing/bad-size/corrupt shards, no provider/transport errors, and enough Reed-Solomon availability may be selected.
- Repair must perform a Full BLAKE3 pre-check before reconstruction even when the preceding scrub was Quick.
- CLI group-scoped repair uses repeated `--group` arguments; the GUI must not duplicate repair safety logic with a different backend path.
- Provider/transport errors are unknown state, not erasures. They block repair for the affected selected group.
- After selective repair, verify the selected groups again; unrelated degraded groups may remain and must stay visible in the persisted integrity snapshot.


### Maintenance metadata ownership

`integrity.json` is advisory/rebuildable state written after scrub and repair scans. The Metadata screen owns manifest-replica operations and inventory rebuild; Diagnostics owns Doctor presentation. Do not make inventory or integrity snapshots authoritative over manifests or remote shards.


## Crypt Secret Portability checkpoint

See `docs/CRYPT_SECRET_PORTABILITY.md` and
`docs/CRYPT_SECRET_PORTABILITY_STATE.json` before continuing. B5 transactional
restoration now supports both rclone-encrypted and ordinary plaintext rclone
configs without creating additional plaintext staging/backup files. B6 real-tool tests are source-complete but have not been executed. B7 top-level
`rpool export` / `rpool import` integration is complete. The current requested
delivery is a full-project ZIP; normal future releases return to changed-file-only
patch ZIPs unless a full project is explicitly requested.


## Crypt Secret Portability checkpoint — B6

B6 adds ignored real-tool integration coverage in `src/config_sync/b6_integration_tests.rs`.
The harness creates isolated temporary rclone configs, local backends, crypt remotes,
age identities and encrypted sentinels. It checks exact obscured-value preservation,
actual pre-existing crypt data decryption after restore, optional password2 behavior,
idempotency, and fail-before-mutation behavior for wrong/missing identities, corrupted
vaults, invalid schema, missing passwords, name mismatches and non-crypt targets.
No B6 test was executed in this environment; only source-level static checks were run.
B7 CLI orchestration is complete. The next planned project work returns to the v0.5.16 Settings / UI Persistence sequence after this portability checkpoint is validated on the user build environment.
# Cross-platform priority (2026-09-23)

Windows is the primary user environment. New work must consider Windows,
macOS and Linux, preserve native path/argument handling, and avoid shell-only
test fixtures. Report executed platforms separately from intended support.
The storage cross-platform GitHub Actions workflow covers all three OSes;
writing that workflow is not evidence that those jobs have passed.
The Step4 LocalBackend remains a Unix-only synthetic prototype; Windows
reparse-safe local implementation is still pending. Step5 RcloneBackend and its
Rust fake-process fixtures use cross-platform std process/I/O APIs.
