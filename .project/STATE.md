# Project State

Updated: 2026-11-18

## Project

RPool is a Rust GUI/CLI sharded archive tool and online drive over encrypted
rclone remotes. Canonical source: `artifacts/rpool`. Build/test only in isolated
working copies under `projects/rpool`, with targets/logs/temp data outside source.
Windows is the primary GUI platform; support macOS and Linux too. This Mac has no
live user pool: local fixtures are not evidence of live-pool or Windows behavior.
Current candidate: 2.23.0-rc.3 (2026-11-18), owner-authorized for prerelease.
The prior 2.23.0-rc.2 prerelease remains published.
The prior 2.23.0-rc.1 prerelease remains published.

## Current Status

Individual storage-location editing candidate 2.23.0-rc.3 (2026-11-18): CLI and GUI now
rename an existing crypt location or change its own backing path. Rename keeps
the old name as an alias to the new crypt so existing archive/event references
continue to resolve, and updates current local pool/GUI/root/domain references
without changing keys. Path edits require a readable empty destination and copy
encrypted bytes, check integrity by reading both sides, then repoint the selected
crypt; source data is never automatically deleted. All PCs/external writers must
be stopped and explicitly acknowledged. Ambiguous/nested/shared roots, unreadable
paths and encrypted rclone configs are refused. Current clients use a per-PC use
lease, but older clients do not honor it. Native macOS release bins built (exit 0);
no tests, smoke, live storage operation or independent review ran. The owner
authorized the rc.3 prerelease; both local release binaries built and report
2.23.0-rc.3. Commit/tag publication is the remaining step, not user installation.

GUI restoration prerelease 2.23.0-rc.2 (2026-11-18): tray actions now show/unminimize/
focus the existing native window before relying on egui repaint or background
polling. macOS Dock reopen uses the same callback without replacing winit's
application delegate. No drive/job restart or settings changes. Both macOS release
binaries and Windows GNU release binaries built successfully; the Windows build
used --no-default-features (WinFsp excluded). Runtime reproduction of the reported
long-background symptom is not established. No tests, GUI exercise or independent
review ran, per the owner's build-only policy. Release v2.23.0-rc.2 is published
from commit 943a2e0. GitHub Actions run 44206697230 succeeded for Windows MSVC
(default WinFsp feature), Linux and both macOS architectures, then published
four archives and SHA256SUMS.txt. The bounded observation and build records are
in projects/rpool/.project/releases/v2.23.0-rc.2/. User installation/remount
was not performed.

Multiple storage locations on one provider connection are implemented and locally
validated in `projects/rpool/provider-multi-locations-20261113`, based on ae385eb.
They reuse named crypt remotes and existing authentication, discovery and pickers.
CLI and GUI can add explicit physical roots without relocating existing data or
changing old crypt keys/defaults/pool membership. The owner authorized GitHub
prerelease v2.23.0-rc.1. Both local release binaries built successfully and report
that version. Commit 18c91c3 and tag v2.23.0-rc.1 were pushed. GitHub Actions run
43364891114 succeeded on Windows, Linux and both macOS architectures and published
the prerelease with four archives plus SHA256SUMS.txt. User installation/remount
was not performed. Publication observation is recorded under
projects/rpool/.project/releases/v2.23.0-rc.1/.

The prior off-lock projection remediation was committed as c7e4d67; release
metadata deb218b and tag v2.22.2 were pushed. Workflow repair ae385eb changed the
macOS runners. Last recorded release evidence (2026-11-09) says the original run
42641164290 ended cancelled: Windows/Linux builds succeeded, both macOS jobs were
cancelled and publication skipped. A same-tag manual dispatch was blocked on
Actions write authentication. Publication has not been live-rechecked in this
feature task; do not claim the release is published or move/recreate its tag.

## Working

- [x] `provider encrypt --path`: omitted uses existing default; explicit empty
  uses connection root. New config appends crypt entries and preserves old bytes.
- [x] GUI Storage locations lists backing roots and adds a named location; the
  legacy default relocation is a separate collapsed operation, not part of adding.
- [x] Existing pool/upload pickers discover each new crypt. No automatic pool edit.
- [x] Explicit SFTP/SMB/local capacity groups can distinguish independent disks;
  same disk must share a group. Account-wide cloud quota cannot be split.
- [x] Conflicting groups for the same lexical concrete root are rejected before
  quota calls. Windows local separators/drive/UNC boundaries and alias suffixes
  have regression coverage. Outage identity/rate limits/shard caps stay on base.
- [x] Existing portable crypt paths and age vault retain locations and keys.
  Import still requires matching target crypt names and encryption structures
  before key restoration; it does not auto-create crypt sections.
- [x] Off-lock projection snapshots/cache and uploaded-commit preparation passed
  prior independent review and local validation; see MOUNT_IDE_PERFORMANCE.md.

## In Progress

- A separate reported migration upload failed at provider-denied shard writes to
  a new location while directory reads succeeded. Source tracing preserves the
  full backing path; exact server ACL/read-only/metadata cause is unconfirmed.
  No permissions, real storage, migrations or mounts were changed by diagnosis.
  This release does not claim to fix the provider write denial.
- Live Windows folder-browse stall/apparent mount interruption remains undiagnosed.
  The owner explicitly restarted/remounted, so PID replacement is not evidence
  of automatic process termination. The older investigation observed 2.22.1;
  the latest installed binary observed for the new upload report was 2.23.0-rc.2.
  Application crash/hang and WinFsp event lookups did not establish a cause.

## Next

1. Publish the owner-authorized v2.23.0-rc.3 prerelease with individual location
   editing. Preserve all prior tags and do not install or remount user systems.
2. If requested, confirm Windows binary/version/mount identity before a bounded
   runtime test. Obtain deployment/remount authority separately.
3. Preserve the older v2.22.2 tag. Its changes are included in the published
   2.23 prerelease; do not automatically retry the old publication workflow.

## Known Issues

- Native Windows execution, actual GUI click/render behavior and live cloud/disk
  capacity are not validated by these macOS tests or cross-target checks.
- Lexical quota identity cannot prove server symlink/bind-mount equivalence or
  independently configured connections. User disk declarations must be truthful.
- GUI domain edits reject stale rows and preserve other mappings, but independent
  processes can still race between load/save; no cross-process transaction added.
- Long-background GUI restoration has a native-window fix in published rc.2,
  but the user symptom is not runtime-confirmed. Offscreen placement and renderer
  failure are not claimed fixed. Cross-PC empty-folder and Windows WinFsp runtime
  behavior remain separate, unverified items.
- Pool sync is eventual, not distributed locking/open-handle coherence.
- macOS NFS server serializes requests; whole-shard downloads can stall lookups.
  macFUSE kernel backend was fixture-tested previously, not FSKit/live cloud.
- Legacy 4 GiB macOS NFS workspace is OS-recovery evidence: do not reuse or clean it.
- S3-like crypt upload key-stat behavior and live migration/adoption remain separate
  work; do not expand the current feature into cleanup or data migration.

## Decisions

Use crypt remotes as location entries, not a new registry/portable format.
Adding is not moving, deleting, hiding keys or changing existing defaults.
Capacity/outage declarations remain PC-local and are not exported; re-declare
after import. A location overrides capacity only, never the base failure domain.
Do not infer separate disks/accounts merely from different paths or remote names.
Preserve running mounts/settings/cloud data. No forced termination, remount,
actual user config edits, deployment or installation during local development.
Do not request real passwords or keys for fixture validation.

## Architecture

- CLI provider/application -> config_sync/provision::CryptSetup.path.
- GUI providers/location + ProviderDetails locations -> existing task refresh.
- storage/admin/catalog resolves backing capacity/outage/placement identities;
  budget rejects conflicting concrete roots before querying conservative quotas.
- location_tests covers declarations, wrappers, aliases and OS path identities.
- Existing portable crypt/vault, pool, manifest and archive formats are unchanged.

## Validation

2026-11-13 final post-review source, macOS Rust 1.99.0:
- All-target suite: 1,227 passed, 0 failed, 66 ignored; process-global closed-pipe
  test separately: 1 passed. Total 1,228 passed (nested subprocess reports are
  not counted again). Both warnings-denied release binaries built.
- Rust 1.98.1 Windows GNU + Linux GNU warnings-denied all-target checks passed.
  Cross compilation is not native execution.
- Changed Rust files: edition-2021 rustfmt and git diff checks passed.
- Korean/Japanese/Chinese locale JSON parsed; 15 new nonempty strings per locale.
- Real local rclone/age: 2 encrypted physical-root roundtrips (4 KiB), original
  config prefix/keys/default preservation, shared-filesystem capacity not doubled,
  and portable roots/vault restoration all passed. Target crypt structures were
  created first, per the existing import contract. No live mount/cloud was used.
- Real local rclone alias chain: 2 backing roots encrypted roundtrip and shared
  capacity conservation passed. Windows rules ran via pure helper on macOS; the
  cfg(windows) budget regression was compile-checked, not executed.
- Independent CLI/GUI review: no blocker. Independent capacity review found two
  defects (Windows separators and alias suffix loss), fixed with six additional
  macOS regressions plus one Windows-only regression. Parent and a separate
  independent narrow fix reviewer found no remaining blocker in the fix.

Evidence: `projects/rpool/provider-locations-validation-20261113/` retains
final-validation.json, real-tool receipts, the runners and historical SHA256
snapshots. Routine build/test logs (including pre-fix duplicates) were removed
at the owner's 2026-11-13 cleanup request; old log hashes refer to removed files.
The recorded 1,228-test result was executed before cleanup, not rerun afterward.

2026-11-13 cleanup: 24 Cargo target directories, routine logs, generated fixture
contents, an obsolete binary and verified duplicate extracted release files were
removed. All 847 canonical and 847 latest-worktree source-file hashes plus Git
status matched before the deliberate STATE-only documentation update. After
that cache cleanup, the owner requested obsolete folders be retired too: 51 old
folders and loose legacy root sources are now preserved in the private backup
.backups/rpool-folders-20261113/ (archive SHA/content/modes verified; independent
Git-bundle clone/fsck passed). The archive includes dirty/untracked source and
standalone/bare repositories; see its RESTORE.md for old linked worktrees.
Only the canonical checkout and latest provider-multi-locations worktree remain
registered (2 total); no branch/tag refs changed. Important diagnostics and the
protected 4 GiB recovery workspace remain in place. Do not auto-recreate retired
paths. projects/rpool is now a container, not a root Cargo checkout.
Build/test caches now need regeneration; run the preserved
validate-final.sh before smoke.py, which uses its release binary. No rebuild
was run merely to refill the deleted caches. Cleanup details are recorded in
projects/rpool/.project/cleanup-20261113.json.

## Resume

Current owner-authorized task: publish v2.23.0-rc.3 with individual storage-location
rename/path editing. Local release build succeeded; commit/push its new tag and
observe GitHub platform builds/publication. Build-only policy: no tests, smoke,
extra checks or independent review. The parallel upload diagnosis found provider
write denial with directory reads working; exact server permissions/read-only
cause is not established and no server change is authorized. Do not claim this
release fixes that environment issue. Preserve all old tags and user mounts/data.
