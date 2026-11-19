# Mixed filename encodings: preparation, not completed recovery

Changing rclone crypt filename_encoding changes the namespace used for future lookups and writes. It does not rename, convert, merge or validate previously stored objects. Returning to base32 can hide base32768 names; selecting base32768 can hide base32 names. The keys and data encryption can be the same while the visible names differ.

## Read-only bounded inventory

Example (run only against an explicitly authorized location):

    rpool provider encoding-inventory --remote office_disk_crypt       --encodings base32,base32768 --json --output new-encoding-report.json

Only a direct, standard-name-encrypted crypt backed by a configured SFTP, SMB or local remote is accepted. Aliases, nested wrappers, cloud backends, unsupported crypt settings and crypt/config environment overrides are rejected. RCLONE_CONFIG can select an existing config; only the unlock password override is otherwise retained.

The command takes a private temporary snapshot of the selected crypt and its direct backing. Local rclone config parsing verifies the reconstructed snapshot has exactly the same options. Each view then uses the same snapshot/key/root with only the per-operation --crypt-filename-encoding flag changed. The live rclone configuration, remote roots, pools, plans, keys and stored objects are not changed. No shared rclone daemon is used. The temporary config contains credentials; it is privately created and removed on normal cleanup, not securely erased, and abrupt machine/process failure can prevent cleanup. No credential values or subprocess stderr enter reports.

Without --output there are no persistent report/history/identity/config-directory/location-lease writes. With --output only a NEW explicit report is created; overwrite and missing parent-directory creation are refused. A failed write may leave a partial report. On Unix reports use mode 0600; on other systems protect the chosen destination with appropriate directory ACLs.

### Scope and bounds

The default scope is the crypt root's virtual-* archive identifiers and the presence/name inventory of .rpool-sync. Archive directories are NOT descended. Metadata contents are NOT opened. Other top-level filenames are not disclosed. Metadata paths and noncanonical virtual identifiers are BLAKE3 path fingerprints rather than personal names; these correlate observations, NOT file content or cryptographic identity.

Defaults:

- --max-depth 2 (allowed 1..8): metadata directory depth below the crypt root.
- --max-entries 10000 (1..100000): processed entries, including out-of-scope root entries.
- --max-listings 64 (2..1024): directory requests; a fair share is reserved for every view.
- --max-listing-bytes 1048576 and --max-output-bytes 1048576 (4096..8388608 each).
- Each child has the existing secret-process 120-second deadline; this is not a 120-second total inventory deadline. The maximum request count also bounds the overall work.

A depth frontier with unvisited metadata directories, request/entry cap, listing failure, unsupported snapshot or output truncation sets incomplete=true and a nonzero exit after the bounded report is emitted. Failed listings have unknown/null counts, never fabricated zero or empty-success. Each encoding has its own provenance, top_complete and metadata_complete fields. Output truncation drops detail arrays but retains bounded summary/counts, marks incompleteness and marks top_overlaps_complete=false. A complete bounded name inventory is still NOT archive integrity verification.

Overlapping logical top names are listed with all observed encodings. They do NOT establish identical files, conflict resolution, completed archives, or completed migration runs. Folder counts must not be summed as unique completed archives. Missing roots, permission failures and offline providers remain incomplete observations rather than evidence of absence. Full manifest/shard checks, content comparison, migration-journal reconciliation and reference validation remain separate authorized work.

## Name-encoding change guard

The ordinary GUI/CLI path only changes EMPTY physical backing locations. Even an empty stored directory makes a backing nonempty. The physical backing is inspected rather than trusting the currently visible crypt namespace. Unreadable or over-limit backing listings fail closed. The GUI no longer offers a single-click nonempty bypass and defaults to base32 rather than presuming a long-path recovery choice.

Legacy scripts using --existing-files-ok alone now fail before mutation, even if the setting is already selected. This intentional compatibility break prevents hidden old/new namespace drift.

An expert recovery operator can still restore a chosen single encoding setting on a nonempty location, but ONLY after reviewing a read-only report and stopping mounts/uploads/jobs on EVERY PC, including offline PCs. The explicit CLI path requires ALL of:

    --existing-files-ok --all-pcs-stopped --old-names-not-converted     --inventory-reviewed --recovery-override

These are independent human acknowledgements, not automatically verified proof that other PCs stopped or that the report is sufficient. A local exclusive operation lease and an optimistic original-config fingerprint check protect the local change. They are not a distributed lock against external rclone/config editors. The override changes the encoding setting only; it does NOT convert names, merge views, verify data, make both formats visible or report recovery completion. It must not be used as an ordinary long-path fix on existing data.

## Still requires explicit decisions/approval

Do not automatically copy, rename, move, delete or consolidate stored names. Inventory can prepare those decisions but does not authorize them. Decide on the future write encoding, per-logical-path content conflicts, source retention, capacity/destination and reference/manifest/journal handling first. Coordinate all PCs and their installed versions; existing immutable plans, portable packages and independently running migrations are not rewritten or silently resumed by this feature. No portable schema or mixed-view read framework is introduced here.
