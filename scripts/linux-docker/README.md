# Linux FUSE verification in Docker

Runs the native FUSE frontend and the pool-sync drive against a real Linux
kernel inside a Docker VM. Nothing is mounted on the host and no cloud account
is used: every script builds rclone crypt remotes over local directories inside
the container. Work from a disposable copy of the source.

```sh
docker build -t rpool-linux-dev scripts/linux-docker
SRC=/path/to/source-copy   # mounted read-only; build output goes to a volume
DOCKER="docker run --rm --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor:unconfined \
  -v $SRC:/src:ro -v rpool-target:/target -v rpool-cargo:/root/.cargo/registry -e CARGO_TARGET_DIR=/target"
$DOCKER -v $SRC/scripts/linux-docker/run-tests.sh:/run.sh:ro rpool-linux-dev /run.sh
$DOCKER -v $SRC/scripts/linux-docker/fuse-e2e.sh:/run.sh:ro rpool-linux-dev /run.sh
```

All mount scripts use the single drive mode,
`rpool mount --pool P --workspace W --mountpoint M [--pool-worker NAME]`.

| Script | What it checks |
|---|---|
| `run-tests.sh` | `cargo fmt --check`, warnings-denied `cargo check`, the ignored FUSE kernel tests, the whole suite, clippy (report-only). |
| `fuse-e2e.sh` | `--frontend fuse`: write, rename, background sync, ciphertext-only remotes, remount and read back. |
| `pool-sync-e2e.sh` | Two PCs (workspaces A and B) on one pool: propagation, delete, concurrent-edit conflicts, atomic save, ciphertext-only remotes. `NATIVE_CRYPT=0` writes through rclone crypt. |
| `cache-recovery-e2e.sh` | A WebDAV mount killed with dirty VFS cache; the next native mount recovers the writes, and a peer edit to an unread file survives next to the recovered copy. |
| `layout-change-e2e.sh` | A pool layout change while an upload is pending: the mount opens, reports the deferral, finishes the upload with the old layout, then uses the new layout. |
| `rclone-import-e2e.sh` | `--import-from` of a plain rclone crypt tree into the drive; source unchanged, rerun idempotent, second PC sees the files. |
| `pool-browse-epoch-e2e.sh` | `--apply-pool-changes` moves the drive to a new metadata generation; a workspace-less `rpool pool browse` shows it. |
| `idle-traffic-e2e.sh` | Idle cloud traffic of a mounted pool (`NATIVE_CRYPT=1|0`, `IDLE_LIMIT`, `ALLOW_REREAD`, `DAEMON`). Fails if an uploaded shard is downloaded more than once or idle download exceeds `IDLE_LIMIT` bytes per remote in 30 s. |
| `monitor-e2e.sh` | Two pools mounted at once; `.rpool/net-status.json`, the mount registry and `rpool mount monitor` (text and JSON). |
| `pool-migrate-plan-e2e.sh` | Read-only migration planner: relocation without loss, exact lost-file list, plan from a fresh HOME. |
| `pool-migrate-e2e.sh` | Migration run killed on PC A and resumed on PC B; replacements restore byte-identical; lost files listed. |
| `migrate-journal-e2e.sh` | Migration cloud journal shared between two PCs; no plaintext names in raw remotes. |
| `migrate-relocate-e2e.sh` | Relocation by verbatim copy (source readable) and by Reed-Solomon rebuild (source gone). |
| `migrate-server-copy-e2e.sh` | Relocation through server-side WebDAV COPY (`rclone serve webdav`). |

Expected idle traffic of `idle-traffic-e2e.sh`: no shard reads, only the sync
poll (`lsjson` of the event folders, which grows with the event history, plus
`cat` of events other PCs added) and the capacity `about` call; about 0.8 KB per
remote per interval with a handful of events.
