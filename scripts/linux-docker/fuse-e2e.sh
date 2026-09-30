#!/bin/sh
# End-to-end: `rpool mount --frontend fuse` with real rclone crypt remotes over
# local directories inside this container. No cloud, no host mounts.
set -eu
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
rclone version | head -1
E=/e2e; rm -rf $E; mkdir -p $E/home $E/ws-parent $E/mnt
export HOME=$E/home
for i in 1 2 3; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
$RPOOL pool set e2e --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1
WS=$E/ws-parent/ws
start() {
  rm -f $E/stop
  $RPOOL mount --virtual-drive --pool e2e --workspace $WS --mountpoint $E/mnt --frontend fuse \
    --stop-file $E/stop --interval-seconds 2 \
    --capacity-domain b1=acct1 --capacity-domain b2=acct2 --capacity-domain b3=acct3 \
    --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3 "$@" > $E/mount.log 2>&1 &
  PID=$!
  for i in $(seq 1 60); do grep -q " fuse" /proc/mounts && grep -q "$E/mnt" /proc/mounts && return 0; kill -0 $PID 2>/dev/null || break; sleep 1; done
  echo "MOUNT FAILED"; cat $E/mount.log; exit 1
}
stop() { touch $E/stop; wait $PID || { echo "MOUNT EXIT $?"; cat $E/mount.log; exit 1; }; }
start
head -c 3000000 /dev/urandom > $E/big.src
cp $E/big.src $E/mnt/big.bin
mkdir $E/mnt/docs; echo "hello rpool fuse" > $E/mnt/docs/note.txt
mv $E/mnt/docs/note.txt $E/mnt/docs/renamed.txt
cmp $E/big.src $E/mnt/big.bin && echo "PASS: live read"
# Wait for background sync to upload the pending intents.
for i in $(seq 1 90); do
  if grep -q "pending=[1-9]" $E/mount.log && grep "pending=" $E/mount.log | tail -1 | grep -q "pending=0"; then break; fi
  sleep 2
done
grep -E "pending=|Virtual sync pending" $E/mount.log | tail -4
stop
echo "PASS: clean stop"
files=$(find $E/data1 $E/data2 $E/data3 -type f | wc -l); echo "encrypted objects: $files"
[ "$files" -gt 0 ] || { echo "FAIL: nothing uploaded"; cat $E/mount.log; exit 1; }
! grep -rq "hello rpool fuse" $E/data1 $E/data2 $E/data3 && echo "PASS: no plaintext in remotes"
start
cmp $E/big.src $E/mnt/big.bin && echo "PASS: read after remount"
[ "$(cat $E/mnt/docs/renamed.txt)" = "hello rpool fuse" ] && echo "PASS: renamed file after remount"
[ ! -e $E/mnt/docs/note.txt ] && echo "PASS: rename source gone"
stop
echo "E2E OK"
