#!/bin/sh
# Pool migration relocation (WP3) against real rclone crypt remotes over local
# directories. RS 2+1 on four remotes; remote c3 leaves the pool.
#   Case B: c3 is still readable -> its shards are copied verbatim.
#   Case A: c3's directory is gone -> its shards are rebuilt (Reed-Solomon).
# Both new archives must restore byte-identical files, reference no c3 object,
# and every pre-existing (old archive) object must stay byte-identical.
# Runs once with rclone crypt writes and once with --native-crypt.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
rclone version | head -1
# HOME is replaced per mode below; keep the toolchain reachable.
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo build --locked --bin rpool 2>&1 | tail -1
cargo test --locked --bin rpool --no-run 2>&1 | tail -1
RPOOL=/target/debug/rpool

relocate() { # archive new-id
  RELOCATE_E2E_MANIFEST=$E/$1.bin.rpool.json RELOCATE_E2E_POOL=new RELOCATE_E2E_ID=$2 \
  RELOCATE_E2E_OUTPUT=$E/$2.json \
    cargo test --locked --bin rpool migration::relocate::tests::e2e_relocate_with_rclone \
      -- --ignored --exact --nocapture > $E/$2.log 2>&1 || { tail -40 $E/$2.log; fail "relocate $1"; }
  grep RELOCATE_RESULT $E/$2.log
}
snapshot() { (cd $E && find data1 data2 data3 data4 -type f -exec sha256sum {} + | sort) }

for MODE in rclone native; do
  echo "== mode: $MODE"
  E=/e2e-relocate-$MODE; rm -rf $E; mkdir -p $E/home $E/cfg
  export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
  NATIVE=""; [ $MODE = native ] && NATIVE=--native-crypt
  for i in 1 2 3 4; do
    mkdir -p $E/data$i
    rclone config create b$i local >/dev/null
    rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
  done
  $RPOOL pool set old --remote c1: --remote c2: --remote c3: --remote c4: \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null
  $RPOOL pool set new --remote c1: --remote c2: --remote c4: \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null
  head -c 5500000 /dev/urandom > $E/a.bin
  head -c 4200000 /dev/urandom > $E/b.bin
  $RPOOL put $E/a.bin --pool old --id arch-a >/dev/null 2>&1 || fail "put a"
  $RPOOL put $E/b.bin --pool old --id arch-b >/dev/null 2>&1 || fail "put b"
  grep -q '"c3:' $E/a.bin.rpool.json || fail "fixture: archive a has no shard on c3"
  grep -q '"c3:' $E/b.bin.rpool.json || fail "fixture: archive b has no shard on c3"
  snapshot > $E/before.txt

  relocate b arch-b-new
  echo "PASS: case B (c3 readable) relocated"
  mv $E/data3 $E/data3.away
  relocate a arch-a-new
  echo "PASS: case A (c3 gone) relocated"
  mv $E/data3.away $E/data3

  for x in a b; do
    grep -q '"c3:' $E/arch-$x-new.json && fail "new archive $x still references c3"
    $RPOOL get $E/arch-$x-new.json $E/$x.out >/dev/null 2>&1 || fail "get $x"
    cmp $E/$x.bin $E/$x.out || fail "restored $x differs"
    # The replica in the cloud restores too (not only the local copy).
    $RPOOL get c1:arch-$x-new/manifest.json $E/$x.cloud.out >/dev/null 2>&1 || fail "get cloud $x"
    cmp $E/$x.bin $E/$x.cloud.out || fail "cloud-restored $x differs"
  done
  echo "PASS: new manifests restore identical bytes (local and replica)"
  snapshot > $E/after.txt
  [ -z "$(comm -23 $E/before.txt $E/after.txt)" ] || { comm -23 $E/before.txt $E/after.txt; fail "old objects changed"; }
  echo "PASS: $(wc -l < $E/before.txt) old objects unchanged; $(($(wc -l < $E/after.txt) - $(wc -l < $E/before.txt))) new objects"
  # The old archives still restore from their own manifests.
  $RPOOL get $E/a.bin.rpool.json $E/a.old.out >/dev/null 2>&1 && cmp $E/a.bin $E/a.old.out || fail "old archive a"
  echo "PASS: old archive still restores"
done
echo "ALL PASS"
