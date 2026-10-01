#!/bin/sh
# A pool layout change while an upload is still pending must not block the
# mount. A gate in front of rclone holds every write to the third account, so
# the first mount is killed (-9) with a started upload plan in its spool. The
# pool then gets a new shard size and parity count. The next mount must open,
# report the deferred change, finish the pending upload with its recorded
# (previous) layout and read it back byte-identical. The mount after that
# applies the new layout to new files. NATIVE_CRYPT=1 (default) or 0.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
rclone version | head -1
NC=""; [ "${NATIVE_CRYPT:-1}" = 1 ] && NC="--native-crypt"
echo "native crypt: ${NATIVE_CRYPT:-1}"
E=/e2e-layout; rm -rf $E; mkdir -p $E/home $E/cfg $E/mnt $E/ctl
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
REAL=$(command -v rclone)
# While $E/gate exists, writes that touch account 3 wait; everything else runs.
cat > $E/rclone-gate <<EOF
#!/bin/sh
case "\$1" in rcat|copyto|moveto)
  for a in "\$@"; do case "\$a" in b3:*|c3:*) while [ -e $E/gate ]; do sleep 0.2; done;; esac; done;;
esac
exec $REAL "\$@"
EOF
chmod +x $E/rclone-gate
R="$RPOOL --rclone $E/rclone-gate"
pool() { # shard-mib parity
  $R pool set lay --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards $2 --shard-mib $1 $NC >/dev/null
}
WS=$E/ws
start() { # log
  rm -f $E/stop
  $R mount --pool lay --workspace $WS --mountpoint $E/mnt --frontend fuse \
    --stop-file $E/stop --status-file $E/ctl/capacity.json --interval-seconds 2 \
    --capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 \
    --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3 > $E/$1.log 2>&1 &
  PID=$!
  for i in $(seq 1 90); do grep -q "$E/mnt " /proc/mounts && return 0; kill -0 $PID 2>/dev/null || break; sleep 1; done
  echo "MOUNT FAILED ($1)"; cat $E/$1.log; exit 1
}
stop() { touch $E/stop; wait $PID || { echo "MOUNT EXIT"; tail -30 $E/$LOG.log; exit 1; }; }
synced() { # log: two fresh reports, the last with nothing pending
  n0=$(grep -c "pending=" $E/$1.log || true)
  for i in $(seq 1 240); do
    n=$(grep -c "pending=" $E/$1.log || true)
    if [ "$n" -ge $((n0 + 2)) ] && grep "pending=" $E/$1.log | tail -1 | grep -q "pending=0"; then return 0; fi
    sleep 1
  done
  echo "SYNC TIMEOUT ($1)"; grep -E "pending|rror" $E/$1.log | tail -10; exit 1
}
manifests() { $REAL lsf -R c1: 2>/dev/null | grep '/manifest.json$' | sort; }
check_layout() { # manifest-path shard-bytes parity(0 = none)
  m=$($REAL cat "c1:$1" | tr -d ' \n')
  echo "$m" | grep -q "\"shard_size\":$2," || fail "$1 shard_size is not $2"
  if [ "$3" = 0 ]; then
    echo "$m" | grep -q '"coding":null' || fail "$1 has coding, expected none"
  else
    echo "$m" | grep -q "\"parity_shards\":$3" || fail "$1 parity is not $3"
  fi
}

# 1. Old layout: 1 MiB shards, 2+1. Kill the mount while account 3 is gated.
pool 1 1
touch $E/gate
LOG=m1; start m1
head -c 3000000 /dev/urandom > $E/old.src
cp $E/old.src $E/mnt/old.bin
for i in $(seq 1 120); do [ -n "$(find $WS/spool -name '*.rpool.upload.json' 2>/dev/null)" ] && break; sleep 1; done
[ -n "$(find $WS/spool -name '*.rpool.upload.json')" ] || { tail -20 $E/m1.log; fail "no upload plan started"; }
sleep 3
pkill -9 -f "rpool --rclone" || true; pkill -9 -f rclone-gate || true; pkill -9 rclone || true
wait $PID 2>/dev/null || true
sleep 1; fusermount -uz $E/mnt 2>/dev/null || umount -l $E/mnt 2>/dev/null || true
rm -f $E/gate
[ -z "$(manifests)" ] || fail "upload finished before the kill"
echo "PASS: mount killed with a pending old-layout upload plan"

# 2. New layout: 2 MiB shards, no parity.
pool 2 0
LOG=m2; start m2
grep -q "Pool layout change is deferred: 1 pending upload(s) use the previous layout" $E/m2.log \
  || { cat $E/m2.log; fail "no deferral notice"; }
[ -f $E/ctl/layout-status.json ] || fail "layout-status.json missing"
grep -q '"pending":1' $E/ctl/layout-status.json || fail "layout-status.json: $(cat $E/ctl/layout-status.json)"
echo "PASS: mount opens after the layout change and reports the deferral"
synced m2
cmp $E/old.src $E/mnt/old.bin || fail "old.bin differs in the mount"
stop
OLD=$(manifests)
[ "$(echo "$OLD" | grep -c .)" = 1 ] || fail "expected one archive, got: $OLD"
check_layout "$OLD" 1048576 1
$R get "c1:$OLD" $E/old.get >/dev/null 2>&1 || fail "rpool get old"
cmp $E/old.src $E/old.get || fail "old.bin cloud copy differs"
echo "PASS: pending upload finished with its recorded layout (1 MiB, 2+1) and reads back identical"

# 3. Nothing pending: the new layout applies, the notice is gone.
LOG=m3; start m3
! grep -q "layout change is deferred" $E/m3.log || fail "still deferred with nothing pending"
[ ! -f $E/ctl/layout-status.json ] || fail "stale layout-status.json"
tr -d ' \n' < $WS/virtual.json | grep -q '"shard_mib":2' || fail "workspace policy not updated: $(cat $WS/virtual.json)"
head -c 5000000 /dev/urandom > $E/new.src
cp $E/new.src $E/mnt/new.bin
synced m3
cmp $E/old.src $E/mnt/old.bin || fail "old.bin differs after relayout"
cmp $E/new.src $E/mnt/new.bin || fail "new.bin differs in the mount"
stop
NEW=$(manifests | grep -vxF "$OLD")
[ "$(echo "$NEW" | grep -c .)" = 1 ] || fail "expected one new archive, got: $NEW"
check_layout "$NEW" 2097152 0
$R get "c1:$NEW" $E/new.get >/dev/null 2>&1 || fail "rpool get new"
cmp $E/new.src $E/new.get || fail "new.bin cloud copy differs"
echo "PASS: new file uses the new layout (2 MiB, no parity) and reads back identical"
! grep -rq "rpool.upload" $E/data1 $E/data2 $E/data3 && echo "PASS: no plaintext names in remotes"
echo "LAYOUT CHANGE E2E OK"
