#!/bin/sh
# Pool change migration, end to end, on local crypt remotes (NATIVE=0|1):
# 1. RS 2+1 over c1..c4; three archives. Remove c4 from the pool and delete
#    its data: plan shows relocation and no loss; run is killed mid-way on
#    PC A, resumed on PC B (fresh HOME, only rclone.conf + pools.json), and
#    every replacement restores byte-identical while originals stay intact.
# 2. Remove c3 as well: exactly the archives with two shards on c3+c4 are
#    listed as lost.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
NATIVE=""; [ "${NATIVE_CRYPT:-1}" = 1 ] && NATIVE="--native-crypt"
echo "native_crypt=${NATIVE_CRYPT:-1}"
E=/e2e-migrate; rm -rf $E; mkdir -p $E/a/home $E/a/cfg $E/b/home/.config/rclone $E/b/cfg $E/src
export HOME=$E/a/home RPOOL_CONFIG_DIR=$E/a/cfg RPOOL_PC_ID=PC-A
for i in 1 2 3 4; do
  mkdir -p $E/data$i
  rclone config create b$i local >/dev/null
  rclone config create c$i crypt remote=b$i:$E/data$i password=$(rclone obscure "pw$i") >/dev/null
done
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --remote c4: --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null
for n in 1 2 3 4 5; do head -c $((1500000 + n * 700000)) /dev/urandom > $E/src/f$n.bin; $RPOOL put $E/src/f$n.bin --pool shared > $E/put$n.log 2>&1 || { cat $E/put$n.log; fail put; }; done
echo "PASS: 5 archives uploaded"
snapshot() { (cd $E && find data1 data2 data3 data4 -type f -exec sha256sum {} + 2>/dev/null | sort); }
snapshot > $E/before-objects.txt

# 1. Remove c4 (account gone).
$RPOOL pool set shared --remote c1: --remote c2: --remote c3: --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE > $E/set1.log 2>&1
grep -q "Stored archives are affected" $E/set1.log || { cat $E/set1.log; fail "pool set hint"; }
mv $E/data4 $E/data4.gone
$RPOOL pool migrate plan shared --download-mib-s 50 --upload-mib-s 50 > $E/plan1.txt 2>&1 || { cat $E/plan1.txt; fail plan; }
cat $E/plan1.txt
ID=$(sed -n 's/.*migration[ _]id[=: ]*\([0-9a-f]\{32\}\).*/\1/p' $E/plan1.txt | head -1)
[ -n "$ID" ] || fail "no migration id in plan output"
$RPOOL pool migrate plan shared --json > /dev/null 2>&1 || true
$RPOOL pool migrate status shared --id $ID --json > $E/st0.json
python3 - $E/st0.json <<'PY' || fail "plan counts"
import json,sys
s=json.load(open(sys.argv[1]))[0]; c=s["counts"]
assert c["lost"]==0 and c["relocate"]>=1 and s["to_move"]>=1, s
print("to_move", s["to_move"], "counts", c)
PY
echo "PASS: plan relocates without loss"

# Run on PC A and kill it after the first archive is switched.
$RPOOL pool migrate run shared --id $ID > $E/runA.log 2>&1 &
RUN=$!
for i in $(seq 1 300); do grep -q "Migration 2/" $E/runA.log && break; kill -0 $RUN 2>/dev/null || break; sleep 0.5; done
kill -9 $RUN 2>/dev/null || true; wait $RUN 2>/dev/null || true
grep -E "Migration [0-9]+/" $E/runA.log | head -5
echo "PASS: PC A killed mid-run"

# PC B: fresh HOME, only the rclone config and the pool definition.
cp $E/a/home/.config/rclone/rclone.conf $E/b/home/.config/rclone/
cp $E/a/cfg/pools.json $E/b/cfg/
export HOME=$E/b/home RPOOL_CONFIG_DIR=$E/b/cfg RPOOL_PC_ID=PC-B
$RPOOL pool migrate status shared > $E/stB.txt 2>&1 || { cat $E/stB.txt; fail "status on PC B"; }
grep -q "$ID" $E/stB.txt || { cat $E/stB.txt; fail "PC B does not see the migration"; }
# PC A died holding a claim; without --take-over PC B must leave it alone.
if $RPOOL pool migrate run shared --id $ID > $E/runB0.log 2>&1; then
  grep -q "claimed by" $E/runB0.log && fail "claimed entry was not reported"
fi
$RPOOL pool migrate run shared --id $ID --take-over > $E/runB.log 2>&1 || { tail -30 $E/runB.log; fail "resume on PC B"; }
tail -3 $E/runB.log
$RPOOL pool migrate status shared --id $ID --json > $E/st1.json
python3 - $E/st1.json <<'PY' || fail "migration not complete"
import json,sys
s=json.load(open(sys.argv[1]))[0]
assert s["complete"] and s["switched"]==s["to_move"], s
assert "PC-A" in s["pcs"] and "PC-B" in s["pcs"], s["pcs"]
print("switched", s["switched"], "pcs", s["pcs"])
PY
echo "PASS: resumed and completed on another PC"

# Every replacement restores byte-identical and uses no c4 shard.
cat $E/a/cfg/migrations/replacements.jsonl $E/b/cfg/migrations/replacements.jsonl 2>/dev/null > $E/repl.jsonl
python3 - $E/repl.jsonl > $E/repl.txt <<'PY'
import json,sys
seen={}
for line in open(sys.argv[1]):
    r=json.loads(line); seen[r["old_archive_id"]]=(r["new_manifest"], r["original_name"])
for old,(new,name) in seen.items(): print(new, name)
PY
n=0
while read new name; do
  $RPOOL get "$new" $E/restored.bin > $E/get.log 2>&1 || { cat $E/get.log; fail "restore $new"; }
  cmp $E/restored.bin $E/src/$name || fail "$name differs after migration"
  rclone cat "$new" | grep -q '"c4:' && fail "$name still references c4"
  n=$((n+1))
done < $E/repl.txt
[ $n -ge 1 ] || fail "no replacements recorded"
echo "PASS: $n replacement(s) restore byte-identical, no c4 shard"
snapshot | grep -v "^.* data4/" > $E/after-objects.txt
grep -v " data4/" $E/before-objects.txt | while read h f; do grep -q "^$h  $f$" $E/after-objects.txt || fail "original object changed or deleted: $f"; done
echo "PASS: original archives untouched"

# 2. c3 leaves the pool and c2's data is lost too: every replacement archive
#    (2+1 over c1..c3) keeps only its c1 shard, so all five are unrecoverable;
#    the originals replaced in step 1 must not be reported again.
export HOME=$E/a/home RPOOL_CONFIG_DIR=$E/a/cfg RPOOL_PC_ID=PC-A
mv $E/data3 $E/data3.gone; mv $E/data2 $E/data2.gone; mkdir -p $E/data2
$RPOOL pool set shared --remote c1: --remote c2: --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null 2>&1
$RPOOL pool migrate plan shared > $E/plan2.txt 2>&1 || { cat $E/plan2.txt; fail plan2; }
grep -q "already replaced" $E/plan2.txt || { cat $E/plan2.txt; fail "replaced originals not skipped"; }
ID2=$(sed -n 's/.*migration[ _]id[=: ]*\([0-9a-f]\{32\}\).*/\1/p' $E/plan2.txt | head -1)
$RPOOL pool migrate lost shared --id $ID2 --json > $E/lost.json
$RPOOL pool migrate lost shared --id $ID2 | head -12
python3 - $E/lost.json <<'PY' || fail "lost list"
import json,sys
lost=json.load(open(sys.argv[1]))
print("lost:", sorted(l["original_name"] for l in lost))
assert len(lost) == 5, len(lost)
for l in lost:
    assert l["archive_id"].startswith("migrate-"), ("original reported", l["archive_id"])
    assert any(g["available"] < g["required_k"] for g in l["groups"]), l
    for g in l["groups"]:
        assert all(m["reason"] in ("remote-removed", "missing") for m in g["missing"]), g
PY
echo "PASS: exactly the unrecoverable archives are listed, with missing shards and reasons"
echo "POOL MIGRATE E2E OK (native=${NATIVE_CRYPT:-1})"
