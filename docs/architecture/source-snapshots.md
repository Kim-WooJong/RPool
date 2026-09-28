# Source snapshots and workspace layout

> Superseded workflow (2026-09-28): edit source directly in `artifacts/rpool` and commit with Git. Do not run the snapshot exporter below or overwrite this repository with the older `projects/rpool` source tree. Build with `CARGO_TARGET_DIR=../../projects/rpool/target` from this repository. Current version policy is [VERSIONING.md](../VERSIONING.md); 0.6.0 supersedes the old version reservation. The text below documents historical delivery only. No automatic remote push.

- Work/build/test: `projects/rpool` (target stays here).
- Source-only Git repository: `artifacts/rpool` (Cargo.toml at repository root).
- Other reviewed project: `projects/initial-setup-review/Initial-setup`.
- ZIPs and validation logs from earlier deliveries are retained in workspace `.backups/rpool-delivery-20260928`, not the Git repository.

Historical snapshots are immutable Git tags `v0.5.15-r1` through `v0.5.15-r4` (imported ZIP history); `v0.5.15-r5` adds Storage scrolling and safe copy-only reprocessing. Each imported r1–r4 tag was checked byte-for-byte against its original ZIP. Later snapshots are verified directly against the project source. The main branch additionally records source-delivery configuration. Cargo version remains 0.5.15.

## Create the next snapshot

From the workspace root, after development and tests:

```sh
python3 scripts/snapshot-rpool.py --message "Describe this source revision"
python3 scripts/snapshot-rpool.py --check
```

The exporter includes the named root source/config/docs files and src, docs, examples, scripts, .github. It excludes build outputs, archives, logs and machine-local state; rejects symlinks and dirty delivery repositories; validates file hashes and executable flags before committing. Add new root source filenames to its allowlist when needed. It never builds or pushes. Avoid building in artifacts: use projects/rpool.

## Connect Git hosting

No remote is configured until an intended repository URL is supplied. In `artifacts/rpool`:

```sh
git remote add origin <repository-url>
git push -u origin main
git push origin --tags
```

Use the hosting service's normal Git credential manager or SSH authentication; never put tokens into remote URLs. Snapshot commits use repository-local `Kim-Woojong <kim.woojong@woojong.kim>` attribution; global Git identity is unchanged. Existing historical project documents retain their original paths as historical evidence; use the paths above for current work.
