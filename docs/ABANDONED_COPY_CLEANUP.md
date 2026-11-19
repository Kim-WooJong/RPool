# Abandoned migration: unused NEW copies (not deployed)

This is **not** `pool migrate retire`. Retire still requires a completed, non-abandoned job and keeps drive revisions. `discard-copies` examines only the exact abandoned migration ID's **new output objects**. It never changes stop/abandon/adoption markers and never deletes originals, folders or prefixes. The installed rc.3 is separate from this unshipped worktree implementation.

## Preview and confirmation

```text
rpool pool migrate discard-copies POOL --id EXACT_ID --json
# alias: cleanup-abandoned; --dry-run is optional (preview is default).

# Usage example only, NOT approval to run deletion:
rpool pool migrate discard-copies POOL --id EXACT_ID --workspace /path/to/workspace --confirm --all-pcs-stopped --all-local-workspaces-listed --preview-token TIMESTAMP:FINGERPRINT --json
```

Use repeated `--workspace` for all local workspaces/backups. Persisted GUI mount profiles and adjacent `.migration-backup-*`/`.pool-backup-*` directories are scanned automatically, across pools. Unregistered workspaces cannot be reliably discovered: the acknowledgement asserts all were supplied, or none exist. Existing workspace locks and an exclusive per-PC operation lease must be available. Every PC's mounts, migrations, uploads, other cleanup processes and writers must remain stopped throughout deletion. This PC cannot atomically fence another PC or external writer. The acknowledgement is an operational precondition, not an automatic global lock.

GUI: Storage → Account changes → Pool change migration → abandoned row → **Discard unused NEW copies**. Preview is separate and read-only. Review exact addresses/hashes, preservation reasons and uncertainty, then explicitly acknowledge writer stop, workspace coverage and permanent deletion. Completed-original cleanup stays separate. KO/JA/ZH interface labels are included; technical diagnostics and CLI remain English.

## Fail-closed scope and limitations

- Re-read the exact cloud plan and abandoned record. Any drive adoption marker refuses this mode. A **mandatory seven days since abandonment** and expiry of recorded claim leases are required. Expired leases alone do not prove writers stopped.
- Cross-check ordinary/drive plans, Claimed/Verified/Switched progress, new_archive_id and new_manifest. Random migrate-* or virtual-* names are **not** creation/absence evidence.
- New runs attempt a write-once `copy-created-<EXACT_ID>.json` document **before** any output write. It binds the unique immutable claim, source manifest closure, plan fingerprint, exact destination roots and namespace bindings, only after observing no objects for that output ID on every destination. Receipt failure preserves future cleanup copies without weakening existing migration behavior.
- **Legacy copies without pre-write absence evidence stay, even verified copies and old unknown writes.** Old journals generally cannot distinguish a reused/pre-existing identical object from a newly created object. There is no guessed deletion or force shortcut. The previously abandoned job may therefore have no eligible deletions; the report says why.
- Reused isolated probes currently support standard crypt directly backed by **SFTP/SMB/local** only. Unsupported cloud/wrapper/crypt settings fail closed. Both base32/base32768 top-level views are queried. Any nonempty alternate view, including metadata-only .rpool-sync, blocks deletion. No merge, rename, key/root change or encoding switch occurs. **Currently observed mixed-encoding locations are not cleanup-ready.**
- Reads use private pinned temporary config snapshots, no daemon and no live config changes. Preview writes no migration cache, identity, lease, cloud record or history. Temporary private config files are removed on drop.
- Read every JSON replica of each observed whole crypt namespace: all manifests, drive generations/checkpoints/history, every other migration regardless of status. Own job progress is creation evidence, not a foreign usage reference. Other jobs are never merged because source epochs match. Inventory and workspace JSON also protect copies. Any query failure or unobserved closure blocks deletion. Original source shard addresses are independently protected.
- Conservative bounds: 16 namespaces; 20,000 objects per namespace; 10,000 cloud JSON documents; 128 migration journals; 128 workspaces; 20,000 local traversal entries; depth 12; 64 MiB per read object; 512 MiB cloud/object plus 512 MiB local workspace read budgets. Each remote child has a 120-second deadline. Observation checks a ten-minute deadline between calls (an in-flight child can finish after it). Bounds refuse deletion; they never certify truncated views as complete. Objects over 64 MiB are retained in this minimal implementation.

## Delete protocol and receipts

The report includes exact pool/ID, plan hash, opaque namespace binding, exact object set/count/bytes/hashes, preservation reasons, scope/uncertainties and bounded completion. Timestamp:fingerprint expires after 15 minutes. Confirmation re-observes state and exact candidate bytes/set; mismatch refuses. Retire's default count/byte mass-delete guard is reused. `--force` bypasses **only that numerical guard**, never ownership, references, originals, grace or uncertainty.

Only pinned `deletefile` for exact allowlisted addresses is issued. Before each delete, re-read references/journals/bindings and remaining objects, then re-check the object's exact size/hash. Confirmation creates local immutable fsynced reservations and per-object intent/outcome receipts under `<config>/discard-copies/<hash(pool,id)>/`. Preview creates none.

Interrupted/failed deletes leave durable unknown-outcome intents. Before any subsequent retry, reconcile every unresolved intent through exact stat and content read. Only actual documented not-found results mean already gone. An unreadable/changed object stops subsequent deletion. No blind retry or following deletion occurs after an unknown result. Ledger overflow refuses rather than automatically pruning.

The local ledger and stop acknowledgement are not distributed conditional-delete transactions. A writer violating the acknowledgement can race the final observation/delete gap. Do not confirm while any PC may write.

## Validation boundary

Authorized validation is **build only**: native macOS release bins and Windows GNU release bins with Rust 1.98.1, locked/offline, no default features for Windows. No tests, smoke execution, check/clippy/fmt-check, independent review, live migration, deletion, install, release, commit or push. Compilation is not live deletion/recovery verification.
