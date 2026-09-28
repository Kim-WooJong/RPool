# Step 8 — Provider administration, GUI and diagnostic boundaries

## Implementation

`storage/admin` owns the separate object-safe `BackendAdmin` and `ToolDiagnostics`
contracts. `RcloneAdmin` uses the existing bounded, sanitized subprocess owner and
captured executable/config context, with a 30-second per-operation deadline. Neither
administrative discovery nor quota/version methods were added to StorageBackend.
Provider health and doctor no longer spawn rclone directly. Legacy storage quota and
configuration functions are composition shims, not second implementations.

The sanitized catalog retains only backend type, backing address and encryption
eligibility. Raw config JSON/secrets do not enter GUI state. Unknown/malformed
no_data_encryption is not advertised as usable crypt. Alias/crypt/chunker traversal
has cycle detection; missing/aggregate/unknown backing scope returns an explicit
error instead of silently dropping a target or falling back to a guessed target.
Windows rooted/drive inputs are rejected as legacy capacity aliases.

Quota target addresses and capacity alias-group IDs are distinct. Allowlisted
account-wide quota backends normalize paths under a single configured backing.
Known aliases share one quota fetch, including health reports; target accessibility
is still probed separately. Distinct configuration sections are NOT evidence of
different accounts. Free-ratio placement therefore limits combined allocation to
the smallest reported free budget and refuses unresolved capacity scopes. This can
underutilize truly independent accounts, but avoids summing unproven shared budgets.
Other direct quota targets may still be reported without assigning a domain ID.

FailureDomainId remains unresolved; it is not derived from a capacity alias group.
Configured-remote concentration can demonstrate Unsafe, otherwise the current
manifest/plan assessment is Unknown, never inferred Safe. Paths under the same
configured remote are counted together. Status exposes unknown and labels the known
concentration as a lower bound. RS drain rejects Unknown/Unsafe unless the existing
explicit --allow-risky is supplied. This is an intentional conservative compatibility
change; different crypt names do not prove independent provider outages.

Doctor accepts optional admin/tool dependencies. `doctor --local-only` checks local
metadata without version/discovery/crypt-pool probes, and reports remote checks as
not performed rather than successful. Default doctor remains legacy-compatible.

GUI usage refresh receives an Arc<dyn BackendAdmin> and keeps all provider I/O in
its existing worker. Pool Save now builds the existing pool-set command and runs it
through TaskRunner instead of invoking crypt inspection on the UI thread. Arguments
preserve Unicode/spaces/leading hyphens; completion uses the captured invocation,
including retry. Success refreshes cached definitions; reload errors are retained.
Pool loading uses cached definitions. Existing local metadata controls remain local;
no native credential/volume editor is introduced.

## Boundary inventory

- Data I/O: StorageReader/StorageWriter → StorageBackend → legacy RcloneBackend.
- Provider discovery, quota, accessibility, pool crypt checks: BackendAdmin adapter.
- Tool version: explicitly selected ToolDiagnostics; absent for local-only doctor.
- GUI rpool child: orchestration, unchanged progress parsing and cancellation.
- config_sync tooling: existing sanitized secret/portability boundary, unchanged.
- remote-root/pool JSON: legacy compatibility metadata; not native volume identity.

## Validation

Executed on macOS arm64:

- `cargo test --locked --offline --bin rpool`: **147 passed, 0 failed, 10 ignored**.
- `cargo build --locked --offline --bin rpool`: **passed**.
- CLI smoke: nonexistent --rclone executable with doctor --local-only --json exits 0;
  rclone checks are informational/skipped, not failed or falsely validated.
- 12 new tests cover independent health/quota results, alias deduplication, sanitized
  catalog, unresolved/cyclic/Windows capacity input, malformed encryption flags,
  native diagnostics, quota parsing, conservative free-ratio budgets, Unknown
  failure-domain semantics, worker-thread discovery, GUI CLI argument round trips,
  and the actual admin/tool adapter through the portable Rust fake executable.
- Expert reviews identified duplicate health quota queries, unproven cross-section
  capacity addition, leading-hyphen GUI names and hidden cache reload errors; fixed.
- 24 test-profile warnings and 60 normal-build warnings remain. Unused compatibility
  surfaces and existing warnings are retained for bounded Step11 cleanup.
- SHA-256 audit: models/schema, crypt-portability code, Cargo files, manifest hash
  domains and GUI task/progress/cancellation implementation unchanged. 23 existing
  Rust files changed, 4 added; the Rust process fixture also gained version/lsf cases.

Windows remains the primary target. Portable source/tests and existing process
controls were preserved, but Windows/Linux execution and interactive GUI operation
were not performed. The existing three-OS CI matrix has not been published/run.
Ten real-tool tests remain ignored; no real rclone account/cloud call occurred.

## Next

Step 9 — optional OpenDAL prototype, with explicit release/feature/MSRV/runtime and
lockfile compatibility checks, default-off native writes and synthetic verification.
