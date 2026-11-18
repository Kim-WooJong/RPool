# Multiple storage locations on one connection

A connected provider can have several independently named encrypted storage
locations, for example `nas:/disk1/rpool` and `nas:/disk2/rpool`. Each location
is a crypt remote wrapping the same connection: authentication is not copied.
Existing remote names, keys, default roots, pool membership and stored archives
are not changed when a location is added. No files are moved.

## GUI

Open Providers → Storage locations on the connected provider. Existing crypt
remote names and their configured physical backing paths are listed together.
Enter a new location name and its physical folder, acknowledge the key-backup
notice, then choose Add storage location. The name must be new. After discovery
refreshes, select either or both locations in the pool/upload destination picker.
Adding a location does not automatically add it to an existing pool.

The collapsed Change the existing default location action is the older relocation
operation. It is not needed to add another disk; follow its stop/move safeguards
if intentionally relocating existing data. Deleting keys/remotes or stored data
is not part of the new locations interface.

## CLI

```sh
rpool provider encrypt --provider nas --name nas_disk2_crypt --path /disk2/rpool
```

An omitted `--path` retains the provider default, as before. An explicit empty
`--path=` chooses the connection root instead of applying that default. A path
on `nas_disk1_crypt:` is an encrypted subfolder of disk 1, not another physical
disk: use a new crypt location pointing directly to the base connection.
The existing pool/upload folder is still appended inside that encrypted target.

## Capacity and outage identity

Location names alone do not prove independent storage. Without declarations,
locations retain the backing provider's shared/conservative capacity group.
For SFTP, SMB and local filesystem backends only, separate physical disks may
be explicitly assigned separate Disk capacity groups in the locations dialog.
Paths on the same disk must use the same group. Never declare two folders on
one disk as independent disks. Same concrete quota roots with conflicting
groups are rejected during capacity admission. This lexical check cannot prove
server-side symlink, bind-mount or independently configured connection identity.

The existing CLI can declare groups when starting a mount:

```text
--capacity-domain=nas_disk1_crypt=disk1 --capacity-domain=nas_disk2_crypt=disk2
```

Account-wide cloud quotas cannot be split into independent folder budgets.
Per-location overrides are rejected for other backends rather than assuming
independence. Outage identity, transfer limits and per-account shard caps still
use the original backing connection: two disks in one server are not two
independent servers. Recalculate pool capacity after editing declarations.

Location creation and capacity declaration are separate local operations.
The existing portable crypt definitions and age-encrypted vault carry the
new backing paths and keys. Existing import requires matching crypt remote names
and structures in the target configuration before restoring keys; connect the
backing provider and recreate those locations with their explicit paths first.
It does not automatically create missing crypt configuration sections.
Capacity/outage declarations remain PC-local, as
before; they are not exported and must be reviewed/redeclared on another PC.
No portable, pool, manifest or archive format was changed.

## Edit an individual existing location

Stop mounts, uploads and jobs on **every PC** first. Neither command automatically
unmounts, kills, restarts or deletes anything. The GUI has prefilled name/path
controls on each existing location row and requires the same confirmation.

```sh
rpool provider location-rename --remote disk2_crypt --name archive_crypt --all-pcs-stopped
rpool provider location-path --remote archive_crypt --path /disk3/rpool --copy-existing --all-pcs-stopped
```

Both accept `--json`. The exact old name must be a crypt section, not a provider or
alias. Rename changes the real crypt name, preserves every option/key byte, and
leaves `disk2_crypt` as `type = alias; remote = archive_crypt:`. Keep that alias:
old archives, drive events, pending plans and wrapper references are not rewritten.
Pools, saved GUI destinations, per-remote roots and capacity declarations acquire
the new name; old roots/domain identities remain for compatibility. The base
account, failure domain, limits and shard policies do not change. The GUI refreshes
provider discovery and selected picker targets. Case-only/same-name renames and
case-insensitive collisions are refused; no section is overwritten.

If a local settings write fails after the atomic crypt+alias install, the command
reports failure and rolls earlier settings writes back when still unchanged.
It deliberately retains both crypt and alias so either old or new references
remain readable; inspect local settings instead of blindly retrying the rename.

Path editing affects only the selected crypt's `remote`. First create a readable,
empty destination folder on the **same direct connection**. Missing, denied and
unknown listings are refused rather than treated as empty. Existing source data
requires explicit `--copy-existing`. RPool copies raw encrypted bytes with
`rclone copy --immutable`, compares through `rclone check --download`, and only then
atomically switches the crypt backing. The original source remains after success;
there is no delete-source option. On failure the configuration is not switched,
and partial destination data is retained with manual recovery guidance. This
runtime download check is a product safety gate, not a development QA command.

Same/nested/shared/overlapping roots, connection/drive roots and ambiguous wrapper
paths are refused. Paths containing dot components, colons or backslashes are
also refused. Container/share backends require child folders within the same
container/share, and source/target must use the same absolute/relative style. Comparison is conservative and case-insensitive. Distinct concrete
connections can still refer to the same physical server; the operator must choose
an independent folder and stop external rclone/cloud writers too. Local registry
and saved-workspace locks/leases are checked without cleanup; unknown/unreadable
state blocks the edit. New RPool CLI jobs share a per-PC kernel lease that
excludes location edits for their entire lifetime. Older/external clients do not share this exclusion protocol.

Encrypted rclone configs are conservatively refused, with no plaintext backup or
automatic decryption. Provisioning and restore locks, original-byte optimistic
checks and same-directory atomic config replacement protect edits; do not run
other config editors concurrently. A private temporary staging file is used only
for an already-plaintext config, reusing the existing provisioning contract.

### Portable restore after a rename

Portable JSON and the age vault formats are unchanged. Before import on another
PC, provision the **matching target crypt name and structure**, and independently
restore every required **legacy name-only alias** for old archives/events. Import
does **not** automatically create those aliases. Back up the renamed rclone config
(including aliases) alongside the encrypted vault; never remove an alias merely
because it no longer appears in the new-location picker.
