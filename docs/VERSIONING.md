# Version policy

User-authorized policy as of 2026-09-28:

- Patch: compatible bug fixes.
- Minor: compatible feature additions.
- Major: intentional incompatible public behavior, configuration or data changes, with migration notes.
- Choose the level from the change, not a mandatory patch → minor → major sequence.
- Group related, validated changes; do not bump for every edit or commit. Ordinary work can accumulate between version changes.
- Update Cargo.toml, the rpool package entry in Cargo.lock, current README and changelog together. Preserve historical version records.
- Keep source directly in Git. No separate source snapshots or ZIP delivery. A local version commit/tag does not authorize remote push or external publication.

Current version: 2.23.0 (2026-11-26; stable). This release repairs the identified migration loss-evidence and validated empty-layout bugs. Publication requires the exact commit to pass native CI and release builds. CI local-crypt/WinFsp fixtures do not replace every real-provider, interactive-GUI or macFUSE-kernel scenario; provider/server errors remain operational failures, not proof of data loss. Continue reporting platform and provider feedback.
