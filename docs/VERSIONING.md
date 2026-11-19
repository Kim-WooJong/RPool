# Version policy

User-authorized policy as of 2026-09-28:

- Patch: compatible bug fixes.
- Minor: compatible feature additions.
- Major: intentional incompatible public behavior, configuration or data changes, with migration notes.
- Choose the level from the change, not a mandatory patch → minor → major sequence.
- Group related, validated changes; do not bump for every edit or commit. Ordinary work can accumulate between version changes.
- Update Cargo.toml, the rpool package entry in Cargo.lock, current README and changelog together. Preserve historical version records.
- Keep source directly in Git. No separate source snapshots or ZIP delivery. A local version commit/tag does not authorize remote push or external publication.

Current version: 2.23.0-rc.6 (2026-11-20; experimental/testing prerelease — see CHANGELOG.md). Build-only validation is not runtime/GUI QA or a claim that all issues are fixed. Windows runtime, protected PC-sync conflicts and intermittent-disconnect causality remain unresolved; stable release requires feedback.
