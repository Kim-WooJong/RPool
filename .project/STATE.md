# Project State

Updated: 2026-11-13

## Project

RPool is a Rust GUI/CLI sharded archive tool and online drive over encrypted
rclone remotes. Canonical source: `artifacts/rpool`. Build/test only in isolated
working copies under `projects/rpool`, with targets/logs/temp data outside source.
Windows is the primary GUI platform; support macOS and Linux too. This Mac has no
live user pool: local fixtures are not evidence of live-pool or Windows behavior.
Version 2.23.0-rc.1 is the owner-authorized prerelease candidate (2026-11-13).

## Current Status

Multiple storage locations on one provider connection are implemented and locally
validated in `projects/rpool/provider-multi-locations-20261113`, based on ae385eb.
They reuse named crypt remotes and existing authentication, discovery and pickers.
CLI and GUI can add explicit physical roots without relocating existing data or
changing old crypt keys/defaults/pool membership. The owner authorized GitHub
prerelease v2.23.0-rc.1. Both local release binaries built successfully and report
that version. Source and release metadata are synchronized for the release commit.
GitHub build/publication is the remaining step; user installation is not authorized.

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

- Live Windows folder-browse stall/apparent mount interruption remains undiagnosed.
  The owner explicitly restarted/remounted, so PID replacement is not evidence
  of automatic process termination. Last recorded installed version was 2.22.1.
  Application crash/hang and WinFsp event lookups did not establish a cause.

## Next

1. Publish the owner-authorized v2.23.0-rc.1 prerelease via a new tag and GitHub
   Actions. Preserve existing tags; do not install or remount user systems.
2. If requested, confirm Windows binary/version/mount identity before a bounded
   runtime test. Obtain deployment/remount authority separately.
3. Resume the earlier release dispatch only after resolving its recorded Actions
   authentication blocker and inspecting live release state; preserve the tag.

## Known Issues

- Native Windows execution, actual GUI click/render behavior and live cloud/disk
  capacity are not validated by these macOS tests or cross-target checks.
- Lexical quota identity cannot prove server symlink/bind-mount equivalence or
  independently configured connections. User disk declarations must be truthful.
- GUI domain edits reject stale rows and preserve other mappings, but independent
  processes can still race between load/save; no cross-process transaction added.
- Earlier minimized-GUI freeze, cross-PC empty-folder behavior and Windows WinFsp
  runtime need targeted user-PC verification; do not claim the feature fixes them.
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

Current owner-authorized task: publish v2.23.0-rc.1 as a GitHub prerelease. Local
release build succeeded. Latest owner directive (2026-11-13 13:15): build success
only; no additional tests, smoke, cross-checks or independent review unless asked.
Previously completed test evidence remains historical; do not rerun those gates.
Commit/push the prepared source and new tag, then confirm GitHub build/publication
completion. Live Windows intermittent disconnect remains unconfirmed; do not claim
it fixed. Preserve current user mounts/configuration and all existing tags.
