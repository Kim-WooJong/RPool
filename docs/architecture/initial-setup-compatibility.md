> Follow-up 2026-09-26: the actual Initial-setup attachment has now been reviewed. Legacy module capture/import passed, but timestamp-dependent auto-sync fingerprints were reproduced. See [the source-project audit](../../../initial-setup-review/COMPATIBILITY-REVIEW.md). The earlier audit below did not include that external project.

# Initial setup compatibility audit

Date: 2026-09-24. Scope: current RPool source and the supplied baseline ZIP; the external initial-setup repository was not available for inspection. Windows execution is deferred to the user.

## Conclusion

The storage pivot preserves the inspected setup integration contract. Existing JSON-only `rpool config export/import` hooks remain supported. A disposable macOS environment successfully exercised both that workflow and the current Nushell package capture/restore workflow with real rclone and age tools.

This is not certification of an unseen setup project or Windows runtime. The root Windows executable was not rebuilt or validated by this audit; use a build matching the reviewed source.

## Two distinct workflows

- JSON-only: synchronize `portable-config.json` using `rpool config export/import`; separately restore the age-encrypted rclone configuration. Portable import preserves the machine-local rclone executable setting.
- Package helper: `rpool-dot-capture <managed_root> <age_recipient>` and `rpool-dot-restore <managed_root> <age_identity>`. Synchronize the full `rpool` subtree, including `config/portable-config.json` and `secrets/rclone.age` when present.
- The package helper is not a drop-in replacement for one-argument JSON-only hooks. These differences already exist in the baseline, rather than being introduced by the storage pivot.

## Package restore prerequisites

Restore provider configuration and matching named crypt remotes before importing a crypt package. Backing path and encryption settings must match; import restores crypt secrets, not provider provisioning. The rclone config must be a regular non-symlink file. Keep the age identity outside the package tree. Ensure age and, unless an explicit recipient is supplied, age-keygen are available. The subprocess environment does not preserve arbitrary RCLONE_* overrides; it preserves RCLONE_CONFIG_PASS.

## Evidence

The following files are byte-identical to the supplied baseline ZIP:

- examples/dot-rpool-sync.nu
- rpool.nu
- src/config/paths.rs
- src/models/portable_config.rs
- src/config_sync/import.rs
- src/config_sync/export.rs

Actual isolated macOS checks passed:

- Legacy JSON export and import.
- Package helper export with the expected directory layout.
- Package helper restore after restoring matching rclone configuration.
- Blank target configuration rejected.
- Symlink configuration rejected.
- Old one-argument invocation rejected, confirming the required helper contract.
- Helper propagates a synthetic external-command exit status of 17 rather than reporting success.

All checks used temporary local configuration and synthetic credentials; no existing user configuration or cloud storage was accessed. Machine-readable results: initial-setup-compatibility.json. No production Rust changes were needed. README's misleading link from JSON-only instructions to the package helper was corrected.
