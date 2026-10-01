#!/bin/sh
# Idle cloud traffic of a mounted pool on a real Linux FUSE mount.
# A pool (RS 2+1 over three local crypt remotes) is mounted, a few MB are
# written, and once the queue is drained and verified the mount is left idle
# for several sync intervals. Per remote `received_bytes`/`sent_bytes` from
# `.rpool/net-status.json` are sampled; idle download must stay bounded by
# small metadata polling (no data shard is read back again).
# rclone is wrapped by a shim that logs the verb and remote paths only;
# RPOOL_RCLONE_DAEMON=0 makes every read a visible subprocess.
# Env: MODE=v6|v7 (v7 adds --pool-retention), NATIVE_CRYPT=1|0, ALLOW_REREAD=1
#      to only report (not fail on) repeated shard downloads during upload,
#      IDLE_LIMIT bytes allowed per remote over the idle window (default 65536),
#      DAEMON=1 to keep reads in `rclone rcd` (then only subprocesses are logged).
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
MODE=${MODE:-v6}; NATIVE=${NATIVE_CRYPT:-1}; LIMIT=${IDLE_LIMIT:-65536}
RETENTION=""; [ "$MODE" = v7 ] && RETENTION="--pool-retention --pool-history-limit 1"
NC=""; [ "$NATIVE" = 1 ] && NC="--native-crypt"
echo "mode: $MODE native_crypt: $NATIVE"
E=/e2e-idle; rm -rf $E; mkdir -p $E/home $E/cfg $E/ws $E/mnt $E/bin
export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg
REAL=$(command -v rclone)
cat > $E/bin/rclone <<EOF
#!/bin/sh
# Log verb + remote paths (args containing ':'), never config or secrets.
v=""; for a in "\$@"; do case "\$a" in --config|--*) ;; *) [ -z "\$v" ] && v="\$a" ;; esac; done
p=""; for a in "\$@"; do case "\$a" in /*) ;; *:*) p="\$p \$a" ;; esac; done
echo "\$(date +%s) \$v\$p" >> $E/rclone.log
exec $REAL "\$@"
EOF
chmod +x $E/bin/rclone
export PATH=$E/bin:$PATH RPOOL_RCLONE_DAEMON=${DAEMON:-0}
for i in 1 2 3; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
$RPOOL pool set idle --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 $NC >/dev/null
D="--capacity-domain b1=a1 --capacity-domain b2=a2 --capacity-domain b3=a3 --failure-domain b1=g1 --failure-domain b2=g2 --failure-domain b3=g3"
$RPOOL mount --virtual-drive --pool-sync $RETENTION --pool idle --workspace $E/ws/A --mountpoint $E/mnt \
  --pool-worker PC-A --stop-file $E/stop --interval-seconds 5 $D > $E/mount.log 2>&1 &
PID=$!
for i in $(seq 1 90); do grep -q "$E/mnt " /proc/mounts && break; sleep 1; done
grep -q "$E/mnt " /proc/mounts || { cat $E/mount.log; fail "mount"; }
head -c 3000000 /dev/urandom > $E/big.src
cp $E/big.src $E/mnt/big.bin
head -c 1500000 /dev/urandom > $E/mnt/second.bin
printf 'hello idle\n' > $E/mnt/doc.txt
sync
S=$E/ws/A/.rpool/net-status.json
for i in $(seq 1 180); do
  if [ -f $S ] && grep -q '"pending_files":0' $S && grep -q '"verified_bytes":[1-9]' $S; then break; fi
  sleep 1
done
grep -q '"pending_files":0' $S || { cat $S; tail -20 $E/mount.log; fail "queue not drained"; }
sleep 12   # let the final publish and readback of this cycle finish
snap() { python3 -c 'import json,sys;json.dump(json.load(open(sys.argv[1])),open(sys.argv[2],"w"))' $1 $2; }
# idle WS LABEL: sample one workspace's counters for 30 s and check the limit.
idle() {
  S=$E/ws/$1/.rpool/net-status.json
  snap $S $E/start-$2.json; L0=$(wc -l < $E/rclone.log)
  for t in 10 20 30; do sleep 10; echo "  $2 t+${t}s: $(python3 -c '
import json,sys
s=json.load(open(sys.argv[1]))
print(" ".join("%s=%d/%d" % (r["remote"], r["received_bytes"], r["sent_bytes"]) for r in s["remotes"]))' $S) (received/sent)"; done
  snap $S $E/end-$2.json
  echo "  rclone calls in that window (all mounts, verb + path):"
  tail -n +$((L0 + 1)) $E/rclone.log | cut -d' ' -f2- | sed -E 's#[A-Za-z0-9_.-]{20,}#<obj>#g' | sort | uniq -c | sort -rn | head -12
  python3 - $E/start-$2.json $E/end-$2.json $LIMIT $2 <<'PY' || fail "$2: idle download above limit"
import json,sys
a={r["remote"]:r for r in json.load(open(sys.argv[1]))["remotes"]}
b={r["remote"]:r for r in json.load(open(sys.argv[2]))["remotes"]}
limit=int(sys.argv[3]); bad=False
for k,r in b.items():
    d=r["received_bytes"]-a.get(k,{}).get("received_bytes",0)
    s=r["sent_bytes"]-a.get(k,{}).get("sent_bytes",0)
    print("  %s idle 30s %s received %d B, sent %d B" % (sys.argv[4],k,d,s))
    bad |= d > limit
sys.exit(1 if bad else 0)
PY
  echo "PASS: $2 idle download per remote <= $LIMIT B over 30 s"
}
reads() { # whole-session reads (cat) per object, most repeated first
  echo "reads per object so far:"
  grep -E '^[0-9]+ cat ' $E/rclone.log | cut -d' ' -f3- | sort | uniq -c | sort -rn | awk '{print $1}' | uniq -c | awk '{printf "  %d object(s) read %d time(s)\n", $1, $2}'
}
echo "upload phase (A): $(python3 -c '
import json,sys
s=json.load(open(sys.argv[1]))
print(" ".join("%s received %d sent %d" % (r["remote"], r["received_bytes"], r["sent_bytes"]) for r in s["remotes"]))' $S)"
reads
# Each uploaded data/parity shard is read back exactly once (its upload
# readback); later re-checks in this process only stat it.
max=$(grep -E '^[0-9]+ cat [^ ]*/(data|parity)/' $E/rclone.log | cut -d' ' -f3- | sort | uniq -c | sort -rn | awk 'NR==1{print $1}')
echo "most reads of one data/parity shard during upload: ${max:-0}"
[ "${ALLOW_REREAD:-0}" = 1 ] || [ "${max:-0}" -le 1 ] || fail "uploaded shards were downloaded ${max} times"
idle A A-alone
# Second PC joins while A stays mounted, reads the data twice, then both idle.
mkdir -p $E/mntB
$RPOOL mount --virtual-drive --pool-sync $RETENTION --pool idle --workspace $E/ws/B --mountpoint $E/mntB \
  --pool-worker PC-B --stop-file $E/stopB --interval-seconds 5 $D > $E/mountB.log 2>&1 &
PIDB=$!
for i in $(seq 1 120); do [ -e $E/mntB/big.bin ] && break; sleep 1; done
cmp $E/big.src $E/mntB/big.bin || fail "big.bin differs on second PC"
cmp $E/big.src $E/mntB/big.bin || fail "big.bin differs on second read"
echo "PASS: second PC reads the data back (twice)"
sleep 12
reads
idle B B-after-read
idle A A-with-peer
reads
[ -d /out ] && cp $E/rclone.log $E/mount.log $E/mountB.log /out/ || true
touch $E/stop $E/stopB
wait $PID || { tail -30 $E/mount.log; fail "mount A exit"; }
wait $PIDB || { tail -30 $E/mountB.log; fail "mount B exit"; }
echo "IDLE TRAFFIC E2E OK"
