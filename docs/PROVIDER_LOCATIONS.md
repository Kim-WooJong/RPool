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
