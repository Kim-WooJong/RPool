# Linux FUSE verification in Docker

Runs the native FUSE frontend against a real Linux kernel inside a Docker VM.
Nothing is mounted on the host. Work from a disposable copy of the source.

```sh
docker build -t rpool-linux-dev scripts/linux-docker
SRC=/path/to/source-copy   # mounted read-only; build output goes to a volume
DOCKER="docker run --rm --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor:unconfined \
  -v $SRC:/src:ro -v rpool-target:/target -v rpool-cargo:/root/.cargo/registry -e CARGO_TARGET_DIR=/target"
$DOCKER -v $SRC/scripts/linux-docker/run-tests.sh:/run.sh:ro rpool-linux-dev /run.sh
$DOCKER -v $SRC/scripts/linux-docker/fuse-e2e.sh:/run.sh:ro rpool-linux-dev /run.sh
```

- `run-tests.sh` runs `cargo fmt --check`, a warnings-denied `cargo check`, the
  ignored FUSE kernel tests, and the whole suite.
- `fuse-e2e.sh` configures three rclone crypt remotes over local directories.
  It then runs `rpool mount --virtual-drive --frontend fuse`, writes, renames
  and waits for the background sync, and checks that the encrypted objects
  contain no plaintext. Finally it remounts and reads the data back.
- `idle-traffic-e2e.sh` (`MODE=v6|v7`, `NATIVE_CRYPT=1|0`) mounts a pool-sync
  pool, writes a few MB, and once the queue is drained samples
  `.rpool/net-status.json` while idle, alone and with a second PC mounted. An
  rclone shim logs verb + remote path of every call (`RPOOL_RCLONE_DAEMON=0`, so
  reads are visible subprocesses). It fails if an uploaded data/parity shard is
  downloaded more than once (its upload readback) or if idle download exceeds
  `IDLE_LIMIT` bytes per remote in 30 s. Expected idle traffic: no shard reads,
  only the sync poll (`lsjson` of the event folders, which grows with the event
  history, plus `cat` of events other PCs added) and the capacity `about` call;
  about 0.8 KB (v6) / 1.6 KB (v7) per remote per interval with a handful of events.
