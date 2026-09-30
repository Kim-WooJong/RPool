#!/bin/sh
# Migration planner (read-only) with local crypt remotes. Three RS 2+1 files
# on four remotes; removing one remote from the pool (and deleting its data)
# must relocate without loss; deleting a second remote's data must list
# exactly the files that became unrecoverable. The plan runs from a fresh HOME
# without inventory too (archives uploaded by another PC), and never changes
# any remote.
#   docker run --rm -v "$PWD":/src:ro -v rpool-target-plan:/target \
#     -e CARGO_TARGET_DIR=/target rpool-linux-dev sh /src/scripts/linux-docker/pool-migrate-plan-e2e.sh
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
command -v jq >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq jq >/dev/null; }
TEST=$(cargo test --locked --no-run --bin rpool --message-format=json 2>/dev/null | jq -r 'select(.executable != null) | .executable' | tail -1)
[ -x "$TEST" ] || fail "test binary not found"
E=/e2e-migrate-plan; rm -rf $E; mkdir -p $E/home $E/cfg $E/in
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3 4; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
# f1 on c1,c2,c3; f2 on c2,c3,c4; f3 on c3,c4,c1 (2 MiB = 2 data + 1 parity).
put() {
  head -c 2097152 /dev/urandom > $E/in/$1
  $RPOOL put $E/in/$1 --remote $2 --remote $3 --remote $4 --data-shards 2 --parity-shards 1 --shard-mib 1 >/dev/null
}
put f1 c1: c2: c3:
put f2 c2: c3: c4:
put f3 c3: c4: c1:
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 >/dev/null
plan() { # out-file [home cfg]
  h=${2:-$E/home}; c=${3:-$E/cfg}
  HOME=$h RPOOL_CONFIG_DIR=$c RPOOL_E2E_POOL=shared RPOOL_E2E_OUT=$1 \
    $TEST --ignored --exact migration::plan::tests::e2e_plan_from_env >$1.log 2>&1 || { cat $1.log; fail "plan"; }
}
action() { jq -r --arg n "$2" '.entries[] | select(.original_name | endswith($n)) | .action' $1; }
snapshot() { (cd $E && find data1 data2 data3 data4 -type f -exec sha256sum {} + 2>/dev/null | sort | sha256sum); }

rm -rf $E/data4; mkdir -p $E/data4
before=$(snapshot)
plan $E/p1.json
jq '.counts, .notes' $E/p1.json
[ "$(jq '.counts.lost' $E/p1.json)" = 0 ] || fail "lost after removing one remote"
[ "$(jq '.counts.relocate' $E/p1.json)" = 2 ] || fail "relocate count"
[ "$(action $E/p1.json f1)" = unaffected ] || fail "f1"
[ "$(action $E/p1.json f2)" = relocate ] || fail "f2"
[ "$(jq '.upload_bytes > 0 and .download_bytes > 0' $E/p1.json)" = true ] || fail "estimate"
echo "PASS: one remote removed -> relocate, no loss"

# Another PC: no inventory, only the rclone config and pools.
F=$E/fresh; mkdir -p $F/home/.config/rclone $F/cfg
cp $E/home/.config/rclone/rclone.conf $F/home/.config/rclone/
cp $E/cfg/pools.json $F/cfg/
plan $E/p2.json $F/home $F/cfg
[ "$(jq '.entries | length' $E/p2.json)" = 3 ] || fail "cloud enumeration found $(jq '.entries | length' $E/p2.json)"
[ "$(jq '.counts.relocate' $E/p2.json)" = 2 ] || fail "fresh relocate count"
echo "PASS: archives found from cloud manifests without inventory"

rm -rf $E/data3; mkdir -p $E/data3
before=$(snapshot)
plan $E/p3.json
jq '.counts' $E/p3.json
[ "$(action $E/p3.json f1)" = unaffected ] || fail "f1 should stay recoverable"
[ "$(action $E/p3.json f2)" = lost ] || fail "f2 not lost"
[ "$(action $E/p3.json f3)" = lost ] || fail "f3 not lost"
[ "$(jq '.counts.lost' $E/p3.json)" = 2 ] || fail "lost count"
jq -e '.entries[] | select(.action == "lost") | .losses[0].missing | length > 0' $E/p3.json >/dev/null || fail "loss detail"
RPOOL_E2E_FULL=1 plan $E/p4.json
[ "$(jq '.counts.lost' $E/p4.json)" = 2 ] || fail "full probe lost count"
[ "$before" = "$(snapshot)" ] || fail "plan changed a remote"
echo "PASS: second remote gone -> exactly f2, f3 lost; remotes unchanged"
echo "POOL MIGRATE PLAN E2E OK"
