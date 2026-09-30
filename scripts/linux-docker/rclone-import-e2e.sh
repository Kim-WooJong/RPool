#!/bin/sh
# Files stored with plain rclone (a crypt remote) are imported into a drive,
# uploaded as pool shards, and read back through a native mount. The source
# must be unchanged. MODE=v6 also checks that a second PC sees the files.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
E=/e2e-import; rm -rf $E; mkdir -p $E/home $E/cfg $E/mnt $E/mntB $E/ws $E/plain $E/tree
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3; do mkdir -p $E/data$i; rclone config create b$i local >/dev/null; rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null; done
rclone config create oldb local >/dev/null
rclone config create old crypt remote=oldb:$E/plain password=$(rclone obscure old) >/dev/null
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 --native-crypt >/dev/null
SYNC=""; [ "${MODE:-local}" = v6 ] && SYNC="--pool-sync --pool-worker PC-A"
DOMAINS="--capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3"
echo "mode: ${MODE:-local}"
# Source tree, stored with plain rclone crypt.
mkdir -p $E/tree/photos/2024 $E/tree/docs/empty-dir
head -c 3000000 /dev/urandom > $E/tree/photos/2024/big.jpg
printf 'hello\n' > $E/tree/docs/a.txt; : > $E/tree/docs/empty.txt; printf 'reserved\n' > $E/tree/docs/CON
printf 'source copy\n' > $E/tree/docs/clash.txt
rclone copy --create-empty-src-dirs $E/tree old:
rclone lsjson -R --hash old: | python3 -c "import json,sys;print(sorted((e['Path'],e.get('Size'),e.get('ModTime')) for e in json.load(sys.stdin)))" > $E/before.txt
# The drive already has docs/clash.txt.
$RPOOL mount --virtual-drive $SYNC --pool shared --workspace $E/ws --mountpoint $E/mnt --interval-seconds 2 --stop-file $E/stop $DOMAINS > $E/m1.log 2>&1 &
for i in $(seq 1 90); do grep -q "$E/mnt " /proc/mounts && break; sleep 1; done
mkdir -p $E/mnt/Imported/old/docs; printf 'drive copy\n' > $E/mnt/Imported/old/docs/clash.txt
touch $E/stop; wait; rm -f $E/stop
$RPOOL mount --virtual-drive $SYNC --pool shared --workspace $E/ws --import-from old: --import-to Imported/old --status-file $E/status.json --stop-file $E/stop $DOMAINS > $E/import.log 2>&1 || { tail -30 $E/import.log; fail "import"; }
grep -E "Import finished|skipped" $E/import.log
grep -q "Import finished: 3 imported, 2 skipped, 0 failed" $E/import.log || fail "unexpected import summary"
python3 -c "import json;s=json.load(open('$E/import-status.json'));assert s['phase']=='done' and s['imported']==3, s"
echo "PASS: import summary and GUI status file"
grep "pending=" $E/import.log | tail -1 | grep -q "pending=0" || fail "import left pending uploads"
echo "PASS: imported files uploaded"
rclone lsjson -R --hash old: | python3 -c "import json,sys;print(sorted((e['Path'],e.get('Size'),e.get('ModTime')) for e in json.load(sys.stdin)))" > $E/after.txt
cmp $E/before.txt $E/after.txt || fail "source changed"
echo "PASS: source unchanged"
# Rerun imports nothing new.
$RPOOL mount --virtual-drive $SYNC --pool shared --workspace $E/ws --import-from old: --import-to Imported/old --stop-file $E/stop $DOMAINS > $E/import2.log 2>&1 || fail "rerun"
grep -q "Import finished: 3 imported, 2 skipped" $E/import2.log || fail "rerun summary: $(grep 'Import finished' $E/import2.log)"
check() { # mountpoint
  M=$1
  cmp $E/tree/photos/2024/big.jpg $M/Imported/old/photos/2024/big.jpg || fail "$M big.jpg"
  [ "$(cat $M/Imported/old/docs/a.txt)" = "hello" ] || fail "$M a.txt"
  [ -f $M/Imported/old/docs/empty.txt ] && [ ! -s $M/Imported/old/docs/empty.txt ] || fail "$M empty.txt"
  # Empty directories are local-only (docs/MOUNT.md), so only this PC has them.
  [ "$M" != $E/mnt ] || [ -d $M/Imported/old/docs/empty-dir ] || fail "$M empty-dir"
  [ "$(cat $M/Imported/old/docs/clash.txt)" = "drive copy" ] || fail "$M clash overwritten"
}
$RPOOL mount --virtual-drive $SYNC --pool shared --workspace $E/ws --mountpoint $E/mnt --interval-seconds 2 --stop-file $E/stop $DOMAINS > $E/m2.log 2>&1 &
for i in $(seq 1 90); do grep -q "$E/mnt " /proc/mounts && break; sleep 1; done
check $E/mnt
echo "PASS: imported files read back through the native mount; existing file kept"
touch $E/stop; wait; rm -f $E/stop
if [ "${MODE:-local}" = v6 ]; then
  $RPOOL mount --virtual-drive --pool-sync --pool-worker PC-B --pool shared --workspace $E/wsB --mountpoint $E/mntB --interval-seconds 2 --stop-file $E/stopB $DOMAINS > $E/mB.log 2>&1 &
  for i in $(seq 1 90); do grep -q "$E/mntB " /proc/mounts && break; sleep 1; done
  for i in $(seq 1 60); do [ -e $E/mntB/Imported/old/docs/a.txt ] && break; sleep 1; done
  check $E/mntB
  echo "PASS: second PC sees the imported files"
  touch $E/stopB; wait
fi
echo "RCLONE IMPORT E2E OK"
