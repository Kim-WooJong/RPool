#!/bin/sh
# Network monitoring of mounted pools on a real Linux FUSE mount.
# Two pools (native crypt, RS 2+1 over three local crypt remotes each) are
# mounted at the same time. While a file is written into each, every mount
# process must keep `<workspace>/.rpool/net-status.json` current (sent,
# acked and verified bytes, queue drained, last sync), register itself under
# `<config>/mounts/`, and `rpool mount monitor` must list both pools. After
# a clean unmount the status file and registry entry are gone and the
# per-minute history (`.rpool/net-history/*.jsonl`) is written.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
E=/e2e-mon; rm -rf $E; mkdir -p $E/home $E/cfg $E/ws
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for p in a b; do
  for i in 1 2 3; do
    mkdir -p $E/data-$p$i
    rclone config create b$p$i local >/dev/null
    rclone config create c$p$i crypt remote=b$p$i:$E/data-$p$i password=$(rclone obscure "pw$p$i") >/dev/null
  done
  $RPOOL pool set pool-$p --remote c${p}1: --remote c${p}2: --remote c${p}3: \
    --data-shards 2 --parity-shards 1 --shard-mib 1 --native-crypt >/dev/null
done

mount_pool() { # NAME
  mkdir -p $E/mnt-$1
  D="--capacity-domain b${1}1=a1 --capacity-domain b${1}2=a2 --capacity-domain b${1}3=a3 --failure-domain b${1}1=g1 --failure-domain b${1}2=g2 --failure-domain b${1}3=g3"
  $RPOOL mount --pool pool-$1 --workspace $E/ws/$1 --mountpoint $E/mnt-$1 \
    --pool-worker PC-A --stop-file $E/stop-$1 --interval-seconds 2 $D > $E/mount-$1.log 2>&1 &
  eval PID_$1=$!
}
mount_pool a
mount_pool b
for p in a b; do
  for i in $(seq 1 90); do grep -q "$E/mnt-$p " /proc/mounts && break; sleep 1; done
  grep -q "$E/mnt-$p " /proc/mounts || { cat $E/mount-$p.log; fail "mount $p"; }
done
echo "PASS: two pools mounted at once"
head -c 3000000 /dev/urandom > $E/mnt-a/big.bin
head -c 2000000 /dev/urandom > $E/mnt-b/big.bin
sync
for p in a b; do
  S=$E/ws/$p/.rpool/net-status.json
  for i in $(seq 1 120); do
    if [ -f $S ] && grep -q '"pending_files":0' $S && grep -q '"last_sync_unix":[0-9]' $S && grep -q '"verified_bytes":[1-9]' $S; then break; fi
    sleep 1
  done
  grep -q '"verified_bytes":[1-9]' $S || { cat $S; tail -20 $E/mount-$p.log; fail "pool $p: no verified bytes"; }
  grep -q '"pending_files":0' $S || { cat $S; fail "pool $p: queue not drained"; }
done
echo "PASS: both mounts report sent/acked/verified traffic and a drained queue"
[ "$(ls $E/cfg/mounts/*.json | wc -l)" -eq 2 ] || { ls $E/cfg/mounts; fail "expected 2 registry entries"; }
$RPOOL mount monitor > $E/monitor.txt
grep -q "Pool pool-a" $E/monitor.txt && grep -q "Pool pool-b" $E/monitor.txt || { cat $E/monitor.txt; fail "monitor does not list both pools"; }
$RPOOL mount monitor --json --history-minutes 5 > $E/monitor.json
python3 - $E/monitor.json <<'PY' || fail "monitor json"
import json,sys
m=json.load(open(sys.argv[1]))
assert sorted(x["entry"]["pool"] for x in m)==["pool-a","pool-b"], m
for x in m:
    s=x["status"]; assert s and all(r["acked_bytes"]<=r["sent_bytes"] for r in s["remotes"]), s
PY
sed -n '1,6p' $E/monitor.txt
echo "PASS: rpool mount monitor lists both pools (text and json)"
for p in a b; do touch $E/stop-$p; done
wait $PID_a || { tail -30 $E/mount-a.log; fail "mount a exit"; }
wait $PID_b || { tail -30 $E/mount-b.log; fail "mount b exit"; }
for p in a b; do
  [ ! -f $E/ws/$p/.rpool/net-status.json ] || fail "pool $p: status left after unmount"
  ls $E/ws/$p/.rpool/net-history/*.jsonl >/dev/null || fail "pool $p: no history"
done
[ -z "$(ls $E/cfg/mounts)" ] || fail "registry left after unmount"
echo "PASS: status and registry removed on unmount, history written"
echo "MONITOR E2E OK"
