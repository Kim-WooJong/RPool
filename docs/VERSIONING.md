# Version policy

User-authorized policy as of 2026-09-28:

- Patch: compatible bug fixes.
- Minor: compatible feature additions.
- Major: intentional incompatible public behavior, configuration or data changes, with migration notes.
- Choose the level from the change, not a mandatory patch → minor → major sequence.
- Group related, validated changes; do not bump for every edit or commit. Ordinary work can accumulate between version changes.
- Update Cargo.toml, the rpool package entry in Cargo.lock, current README and changelog together. Preserve historical version records.
- Keep source directly in Git. No separate source snapshots or ZIP delivery. A local version commit/tag does not authorize remote push or external publication.

Current version: 1.0.0 (2026-10-01; major because the old drive modes were removed — see the migration notes in CHANGELOG.md). Local testing does not imply Windows runtime or cloud qualification.
