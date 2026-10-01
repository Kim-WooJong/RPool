#!/bin/sh
# Two "PCs" (workspaces A and B) mount one pool through the default
# `auto` frontend (native FUSE on Linux) inside this container: propagation,
# delete, concurrent-edit conflicts, atomic save, and ciphertext-only remotes.
# NATIVE_CRYPT=0 writes through rclone crypt instead of native crypt.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
rclone version | head -1
NC="--native-crypt"; [ "${NATIVE_CRYPT:-1}" = 0 ] && NC=""
echo "native_crypt: ${NATIVE_CRYPT:-1}"
E=/e2e-pool; rm -rf $E; mkdir -p $E/home $E/cfg $E/mntA $E/mntB $E/ws
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
for i in 1 2 3; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 $NC >/dev/null
DOMAINS="--capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3"
start() { # name
  rm -f $E/stop$1
  $RPOOL mount --pool shared --workspace $E/ws/$1 --mountpoint $E/mnt$1 \
    --pool-worker PC-$1 --stop-file $E/stop$1 --interval-seconds 2 $DOMAINS > $E/mount$1.log 2>&1 &
  eval PID$1=$!
  for i in $(seq 1 90); do grep -q "$E/mnt$1 " /proc/mounts && return 0; sleep 1; done
  echo "MOUNT $1 FAILED"; cat $E/mount$1.log; exit 1
}
stop() { touch $E/stop$1; eval "wait \$PID$1" || { echo "MOUNT $1 EXIT"; tail -30 $E/mount$1.log; exit 1; }; }
synced() { # name: wait for two fresh reports, the last with no pending writes
  n0=$(grep -c "pending=" $E/mount$1.log || true)
  for i in $(seq 1 180); do
    n=$(grep -c "pending=" $E/mount$1.log || true)
    if [ "$n" -ge $((n0 + 2)) ] && grep "pending=" $E/mount$1.log | tail -1 | grep -q "pending=0"; then return 0; fi
    sleep 1
  done
  echo "SYNC $1 TIMEOUT"; grep -E "pending|sync pending" $E/mount$1.log | tail -5; exit 1
}
appear() { # path: wait until it exists
  for i in $(seq 1 120); do [ -e "$1" ] && return 0; sleep 1; done
  echo "NOT PROPAGATED: $1"; ls -la "$(dirname "$1")"; exit 1
}
gone() { for i in $(seq 1 120); do [ ! -e "$1" ] && return 0; sleep 1; done; echo "NOT DELETED: $1"; exit 1; }

start A
grep -q "Native Fuse" $E/mountA.log && echo "PASS: A mounted natively (auto frontend)"
head -c 3000000 /dev/urandom > $E/big.src
cp $E/big.src $E/mntA/big.bin
printf 'SECRET-MARKER shared v1\n' > $E/mntA/doc.txt
synced A
start B
appear $E/mntB/big.bin
cmp $E/big.src $E/mntB/big.bin || fail "B big.bin differs"
echo "PASS: B reads A's file from the pool"
appear $E/mntB/doc.txt
for i in $(seq 1 120); do [ "$(cat $E/mntB/doc.txt)" = "SECRET-MARKER shared v1" ] && break; sleep 1; done
got=$(cat $E/mntB/doc.txt); [ "$got" = "SECRET-MARKER shared v1" ] || fail "B read doc.txt as [$got]; A has [$(cat $E/mntA/doc.txt)]"
echo "PASS: B reads A's text"

# Atomic save on B (temp + rename over), then A sees it.
printf 'saved by B\n' > $E/mntB/doc.txt.tmp && mv $E/mntB/doc.txt.tmp $E/mntB/doc.txt
synced B
for i in $(seq 1 120); do [ "$(cat $E/mntA/doc.txt 2>/dev/null)" = "saved by B" ] && break; sleep 1; done
got=$(cat $E/mntA/doc.txt); [ "$got" = "saved by B" ] || { ls -la $E/mntA $E/mntB; grep -iE "error|fail|sync pending" $E/mountA.log $E/mountB.log | tail -10; fail "A reads doc.txt as [$got]; B has [$(cat $E/mntB/doc.txt)]"; }
echo "PASS: B's atomic save reaches A"

# Delete on A propagates to B.
rm $E/mntA/big.bin
synced A
gone $E/mntB/big.bin && echo "PASS: A's delete reaches B"

# Sequential edit: B reads A's latest first, so B's save simply follows it.
printf 'edit from A\n' > $E/mntA/doc.txt
synced A
for i in $(seq 1 120); do [ "$(cat $E/mntB/doc.txt)" = "edit from A" ] && break; sleep 1; done
printf 'follow-up from B\n' > $E/mntB/doc.txt
synced B
for i in $(seq 1 120); do [ "$(cat $E/mntA/doc.txt)" = "follow-up from B" ] && break; sleep 1; done
[ "$(ls $E/mntA | grep -c '^doc')" = 1 ] || fail "a sequential edit created a conflict: $(ls $E/mntA)"
echo "PASS: sequential edit across PCs, no conflict copy"

# Concurrent edits: both read the same version, then both save before syncing.
cat $E/mntA/doc.txt >/dev/null; cat $E/mntB/doc.txt >/dev/null
printf 'edit from A\n' > $E/mntA/doc.txt & W1=$!
printf 'edit from B\n' > $E/mntB/doc.txt & W2=$!
wait $W1 $W2
synced A; synced B
sleep 5
found=0
for i in $(seq 1 120); do
  n=$(ls $E/mntA | grep -c '^doc' || true)
  if ls $E/mntA | grep '^doc' | xargs -I{} cat "$E/mntA/{}" 2>/dev/null | grep -q "edit from B" && \
     ls $E/mntA | grep '^doc' | xargs -I{} cat "$E/mntA/{}" 2>/dev/null | grep -q "edit from A"; then found=1; break; fi
  sleep 1
done
ls $E/mntA
[ $found = 1 ] || fail "concurrent edits not both visible on A"
echo "PASS: concurrent edits both preserved (A sees both)"
stop A; stop B
echo "PASS: clean stops"
objects=$(find $E/data1 $E/data2 $E/data3 -type f | wc -l); echo "remote objects: $objects"
! grep -rq "SECRET-MARKER" $E/data1 $E/data2 $E/data3 && echo "PASS: no plaintext in remotes"
! find $E/data1 $E/data2 $E/data3 | grep -qE "doc\.txt|big\.bin|manifest|pool-sync" && echo "PASS: no plaintext names in remotes"
echo "POOL SYNC E2E OK"
