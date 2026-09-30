#!/bin/sh
# Migration cloud journal over real rclone crypt remotes (local dirs), with
# native crypt off and on: PC A (saved pool + config-dir cache) publishes a
# plan and a record, PC B (own cache) reads it, appends, and both see the
# union. The raw remote directories must contain no plaintext names or ids.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
# The test changes HOME; keep the toolchain reachable.
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
for NATIVE in 0 1; do
  E=/e2e-journal-$NATIVE; rm -rf $E; mkdir -p $E/home $E/cfg $E/cacheB
  export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
  for i in 1 2 3; do
    mkdir -p $E/data$i
    rclone config create b$i local >/dev/null
    rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
  done
  FLAG=""; [ $NATIVE = 1 ] && FLAG="--native-crypt"
  $RPOOL pool set jpool --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 $FLAG >/dev/null
  RPOOL_JOURNAL_NATIVE=$NATIVE RPOOL_JOURNAL_CACHE_B=$E/cacheB \
    cargo test --locked --bin rpool migration::journal::tests::e2e_two_pcs_over_crypt_remotes -- --ignored --exact --nocapture > $E/test.log 2>&1 \
    || { tail -40 $E/test.log; fail "journal e2e native=$NATIVE"; }
  grep -q "E2E JOURNAL OK native=$NATIVE" $E/test.log || { tail -30 $E/test.log; fail "no OK line"; }
  for i in 1 2 3; do
    n=$(find $E/data$i -type f | wc -l)
    [ "$n" -ge 3 ] || fail "replica $i has only $n objects"
  done
  for word in mig-secret-name archive-secret-id-4242 jpool plan.json records migrations-v1 rpool-sync pc-a; do
    if grep -rqa "$word" $E/data1 $E/data2 $E/data3; then fail "plaintext '$word' in remote content"; fi
    if find $E/data1 $E/data2 $E/data3 | grep -q "$word"; then fail "plaintext '$word' in remote names"; fi
  done
  [ -f "$E/cfg/migrations/jpool/mig-secret-name/plan.json" ] || fail "PC A cache plan"
  [ "$(ls $E/cacheB/jpool/mig-secret-name/records | wc -l)" = 2 ] || fail "PC B cache records"
  echo "PASS: native_crypt=$NATIVE journal replicated to 3 remotes, ciphertext only"
done
echo "MIGRATE JOURNAL E2E OK"
