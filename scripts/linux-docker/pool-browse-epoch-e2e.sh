#!/bin/sh
# A pool drive is moved to a new metadata generation with "Apply pool
# changes" (an account is added); a later write lands in the new generation.
# A workspace-less `pool browse` from a fresh HOME must show the new
# generation, and leave the remotes byte-identical.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
E=/e2e-epoch; rm -rf $E; mkdir -p $E/home $E/cfg $E/mnt $E/ws
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3 4; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 --native-crypt >/dev/null
DOMAINS="--capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 --capacity-domain b4=a4 --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3 --failure-domain b4=g4"
mount_ws() {
  rm -f $E/stop
  $RPOOL mount --pool shared --workspace $E/ws/A --mountpoint $E/mnt \
    --pool-worker PC-A --stop-file $E/stop --interval-seconds 2 $DOMAINS > $E/$1.log 2>&1 &
  PID=$!
  for i in $(seq 1 90); do grep -q "$E/mnt " /proc/mounts && return 0; sleep 1; done
  cat $E/$1.log; fail "mount $1"
}
drain() { # log: two fresh reports, the last with no pending writes
  n0=$(grep -c "pending=" $E/$1.log || true)
  for i in $(seq 1 180); do
    n=$(grep -c "pending=" $E/$1.log || true)
    if [ "$n" -ge $((n0 + 2)) ] && grep "pending=" $E/$1.log | tail -1 | grep -q "pending=0"; then return 0; fi
    sleep 1
  done
  fail "sync $1"
}
mount_ws m1
mkdir -p $E/mnt/docs; printf 'before\n' > $E/mnt/docs/old.txt
drain m1; touch $E/stop; wait $PID
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --remote c4: --data-shards 2 --parity-shards 1 --shard-mib 1 --native-crypt >/dev/null
$RPOOL mount --pool shared --workspace $E/ws/A --apply-pool-changes $DOMAINS > $E/apply.log 2>&1 || { tail -20 $E/apply.log; fail "apply pool changes"; }
grep -q '"epoch"' $E/ws/A/virtual.json || fail "no epoch after apply"
echo "PASS: pool change applied (new metadata generation)"
mount_ws m2
printf 'after the change\n' > $E/mnt/docs/new.txt
drain m2; touch $E/stop; wait $PID
# A stop does not run a final sync; publish what is still local, as the next
# start or background pass would.
$RPOOL mount --pool shared --workspace $E/ws/A --sync-only $DOMAINS > $E/sync.log 2>&1 || { tail -20 $E/sync.log; fail "sync-only"; }
F=$E/fresh; mkdir -p $F/home/.config/rclone $F/cfg
cp $E/home/.config/rclone/rclone.conf $F/home/.config/rclone/
cp $E/cfg/pools.json $F/cfg/
before=$(cd $E && find data1 data2 data3 data4 -type f -exec sha256sum {} + | sort | sha256sum)
HOME=$F/home RPOOL_CONFIG_DIR=$F/cfg $RPOOL pool browse shared > $E/browse.txt 2>$E/browse.err || { cat $E/browse.err; fail "browse"; }
cat $E/browse.txt; cat $E/browse.err
after=$(cd $E && find data1 data2 data3 data4 -type f -exec sha256sum {} + | sort | sha256sum)
[ "$before" = "$after" ] || fail "remotes changed by browse"
grep -q "mode=v6" $E/browse.err || fail "mode"
grep -qx "17 docs/new.txt" $E/browse.txt || fail "file written after the change is missing"
grep -qx "7 docs/old.txt" $E/browse.txt || fail "file carried into the new generation is missing"
grep -q "metadata generation" $E/browse.txt || fail "generation note"
echo "PASS: workspace-less browse shows the newest generation; remotes unchanged"
echo "BROWSE EPOCH E2E OK"
