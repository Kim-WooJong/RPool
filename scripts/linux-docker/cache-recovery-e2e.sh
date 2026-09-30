#!/bin/sh
# A WebDAV mount is killed (-9) with writes still in rclone's VFS cache; the
# next mount (native FUSE via `auto`) must recover them. MODE=v6 also checks
# that an unread file changed by a peer is kept as a recovered copy.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
E=/e2e-cache; rm -rf $E; mkdir -p $E/home $E/cfg $E/mnt $E/mntP $E/ws
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3; do mkdir -p $E/data$i; rclone config create b$i local >/dev/null; rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null; done
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 --native-crypt >/dev/null
SYNC=""; [ "${MODE:-local}" = v6 ] && SYNC="--pool-sync --pool-worker PC-A"
DOMAINS="--capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3"
echo "mode: ${MODE:-local}"
mount_ws() { # workspace mountpoint frontend log extra...
  ws=$1 mp=$2 fe=$3 log=$4; shift 4
  rm -f $ws.stop
  $RPOOL mount --virtual-drive $SYNC "$@" --pool shared --workspace $ws --mountpoint $mp --frontend $fe --interval-seconds 2 --stop-file $ws.stop $DOMAINS > $log 2>&1 &
  for i in $(seq 1 90); do grep -q "$mp " /proc/mounts && return 0; sleep 1; done
  cat $log; fail "mount $mp"
}
synced() { # log: wait for two fresh reports, the last with no pending writes
  n0=$(grep -c "pending=" $1 || true)
  for i in $(seq 1 180); do
    n=$(grep -c "pending=" $1 || true)
    if [ "$n" -ge $((n0 + 2)) ] && grep "pending=" $1 | tail -1 | grep -q "pending=0"; then return 0; fi
    sleep 1
  done
  fail "sync $1"
}
mount_ws $E/ws $E/mnt dav $E/m1.log
printf 'v1\n' > $E/mnt/edit.txt; printf 'peer target v1\n' > $E/mnt/doc.txt
synced $E/m1.log; touch $E/ws.stop; wait
mount_ws $E/ws $E/mnt dav $E/m2.log
synced $E/m2.log # session 1's writes reach the pool, so edit.txt is read from the cloud
cat $E/mnt/edit.txt >/dev/null
printf 'edited unsaved\n' > $E/mnt/edit.txt
printf 'new unsaved\n' > $E/mnt/new.txt
printf 'my unread overwrite\n' > $E/mnt/doc.txt
head -c 2000000 /dev/urandom > $E/big.src; cp $E/big.src $E/mnt/big.bin
sleep 2
pkill -9 -f "rpool mount"; pkill -9 rclone; sleep 1; fusermount -uz $E/mnt 2>/dev/null || umount -l $E/mnt || true
grep -rq '"Dirty": true' $E/ws/vfs-cache/vfsMeta || fail "no dirty cache entries left behind"
echo "PASS: killed WebDAV mount left dirty cache entries"
[ -n "${DEBUG:-}" ] && python3 -c "import json;n=json.load(open('$E/ws/namespace.json'));p=n.get('payload',n);p=json.loads(p) if isinstance(p,str) else p;print('bases',p.get('bases'))"
rm -f $E/ws/.rpool/mount-process.json
if [ "${MODE:-local}" = v6 ]; then
  # A peer changes doc.txt while this PC is down.
  SYNC="--pool-sync --pool-worker PC-B" mount_ws $E/wsP $E/mntP auto $E/p.log
  for i in $(seq 1 60); do [ -e $E/mntP/doc.txt ] && break; sleep 1; done
  printf 'peer edit\n' > $E/mntP/doc.txt; synced $E/p.log; touch $E/wsP.stop; wait
fi
mount_ws $E/ws $E/mnt auto $E/m3.log
grep -q "Native Fuse" $E/m3.log || fail "not native"
grep "Recovered unsaved WebDAV cache" $E/m3.log || fail "no recovery report"
[ "$(cat $E/mnt/edit.txt)" = "edited unsaved" ] || fail "edit.txt is [$(cat $E/mnt/edit.txt)]"
[ "$(cat $E/mnt/new.txt)" = "new unsaved" ] || fail "new.txt"
cmp $E/big.src $E/mnt/big.bin || fail "big.bin differs"
echo "PASS: unsaved writes recovered after switching to native"
if [ "${MODE:-local}" = v6 ]; then
  # Whether rclone read doc.txt decides between an in-place write (then a v6
  # conflict with the peer) and a recovered copy; either way both survive.
  has() { for i in $(seq 1 60); do cat $E/mnt/doc* 2>/dev/null | grep -qx "$1" && return 0; sleep 1; done; ls -la $E/mnt; fail "lost: $1"; }
  has "peer edit"; has "my unread overwrite"
  echo "PASS: peer edit and the recovered overwrite both preserved: $(ls $E/mnt | grep '^doc' | tr '\n' ' ')"
else
  [ "$(cat $E/mnt/doc.txt)" = "my unread overwrite" ] || fail "doc.txt"
fi
synced $E/m3.log; touch $E/ws.stop; wait
ls $E/ws/recovered-native-cache 2>/dev/null | grep -q . && fail "cache folder left although nothing was kept"
echo "PASS: recovered cache folder removed"
echo "CACHE RECOVERY E2E OK"
