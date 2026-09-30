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
