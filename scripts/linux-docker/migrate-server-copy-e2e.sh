#!/bin/sh
# Pool change migration phase 2: relocation copies kept shards server-side.
# Crypt remotes c1..c4 over four directories of a local `rclone serve webdav`
# (`--etag-hash auto`, vendor owncloud): WebDAV COPY is a server-side copy and
# the md5 of every stored object is reported. rclone's local backend has no
# server-side copy on Linux (only on macOS), and `rclone serve s3` cannot be
# used because RPool's put treats a missing S3 key as a directory.
# RS 2+1, three archives; c4 leaves the pool.
#   SCENARIO=readable: c4 still readable -> its shards are streamed to c1..c3.
#   SCENARIO=gone:     c4's data is gone -> its shards are rebuilt.
# Each scenario runs twice from the same start: with RPOOL_MIGRATE_NO_COPY=1
# (phase 1: download + upload of every shard) and with server-side copies.
# The bytes that passed through the PC must drop sharply, every replacement
# must restore byte-identical with no c4 shard, and the old objects must stay
# unchanged. Runs with rclone crypt writes and with --native-crypt.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
cd /src
command -v rclone >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq unzip >/dev/null && curl -sS https://rclone.org/install.sh | bash >/dev/null; }
rclone version | head -1
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
cargo build --locked --bin rpool 2>&1 | tail -1
RPOOL=/target/debug/rpool

snapshot() { (cd $E/s && find bucket1 bucket2 bucket3 bucket4 -type f -exec sha256sum {} + 2>/dev/null | sort); }
SERVER=""
stop_server() { [ -n "$SERVER" ] && { kill $SERVER 2>/dev/null; wait $SERVER 2>/dev/null || true; }; SERVER=""; return 0; }
trap stop_server EXIT
# Sum a key=value field of the "[relocate] ... downloaded=N ..." lines.
field() { sed -n "s/.*\[relocate\].* $1=\([0-9]*\).*/\1/p" $E/run.log | awk '{s+=$1} END {print s+0}'; }
hashed() { sed -n 's/.*\[relocate\].* hash_verified=\([0-9]*\)\/.*/\1/p' $E/run.log | awk '{s+=$1} END {print s+0}'; }

# setup MODE SCENARIO NATIVE: fresh pool, three archives, c4 removed.
setup() {
  stop_server
  E=/e2e-server-copy-$3-$2-$1; rm -rf $E; mkdir -p $E/home $E/cfg $E/src $E/s/bucket1 $E/s/bucket2 $E/s/bucket3 $E/s/bucket4
  export HOME=$E/home RPOOL_CONFIG_DIR=$E/cfg RPOOL_PC_ID=PC-A
  NATIVE=""; [ $3 = native ] && NATIVE=--native-crypt
  rclone serve webdav $E/s --addr 127.0.0.1:18081 --etag-hash auto > $E/serve.log 2>&1 &
  SERVER=$!
  rclone config create w webdav url=http://127.0.0.1:18081 vendor=owncloud >/dev/null
  for i in $(seq 1 50); do rclone lsd w: >/dev/null 2>&1 && break; sleep 0.2; done
  rclone lsd w: >/dev/null 2>&1 || { cat $E/serve.log; fail "webdav server"; }
  for i in 1 2 3 4; do
    rclone config create c$i crypt remote=w:bucket$i password=$(rclone obscure "pw$i") >/dev/null
  done
  $RPOOL pool set shared --remote c1: --remote c2: --remote c3: --remote c4: \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null
  # Same content in every run (deterministic), so byte counts compare.
  # One worker: `rclone serve webdav` answers 423 Locked when parallel uploads
  # create the same parent folder at once (a test-server limit, not RPool's).
  for n in 1 2 3; do
    python3 -c "import random,sys; random.seed($n); sys.stdout.buffer.write(random.randbytes($((2500000 + n * 1300000))))" > $E/src/f$n.bin
    RCLONE_LOG_FILE=$E/rclone-put.log RCLONE_LOG_LEVEL=NOTICE $RPOOL put $E/src/f$n.bin --pool shared --workers 1 > $E/put$n.log 2>&1 || { cat $E/put$n.log; grep -v "not found - using defaults" $E/rclone-put.log | tail -20; tail -20 $E/serve.log; fail put; }
  done
  snapshot > $E/before.txt
  $RPOOL pool set shared --remote c1: --remote c2: --remote c3: \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null 2>&1
  [ $2 = gone ] && mv $E/s/bucket4 $E/bucket4.gone
  return 0
}

migrate() { # MODE
  $RPOOL pool migrate plan shared > $E/plan.txt 2>&1 || { cat $E/plan.txt; fail plan; }
  ID=$(sed -n 's/.*migration[ _]id[=: ]*\([0-9a-f]\{32\}\).*/\1/p' $E/plan.txt | head -1)
  [ -n "$ID" ] || { cat $E/plan.txt; fail "no migration id"; }
  if [ $1 = nocopy ]; then
    RPOOL_MIGRATE_NO_COPY=1 $RPOOL pool migrate run shared --id $ID > $E/run.log 2>&1 || { tail -30 $E/run.log; fail run; }
  else
    $RPOOL pool migrate run shared --id $ID > $E/run.log 2>&1 || { tail -30 $E/run.log; fail run; }
  fi
  $RPOOL pool migrate status shared --id $ID --json > $E/st.json
  python3 - $E/st.json <<'PY' || fail "migration not complete"
import json,sys
s=json.load(open(sys.argv[1]))[0]
assert s["complete"] and s["switched"]==s["to_move"] and s["to_move"]>=1, s
PY
}

check_result() {
  python3 - $E/cfg/migrations/replacements.jsonl > $E/repl.txt <<'PY'
import json,sys
seen={}
for line in open(sys.argv[1]):
    r=json.loads(line); seen[r["old_archive_id"]]=(r["new_manifest"], r["original_name"])
for new,name in seen.values(): print(new, name)
PY
  n=0
  while read new name; do
    $RPOOL get "$new" $E/restored.bin > $E/get.log 2>&1 || { cat $E/get.log; fail "restore $new"; }
    cmp $E/restored.bin $E/src/$name || fail "$name differs after migration"
    rclone cat "$new" | grep -q '"c4:' && fail "$name still references c4"
    n=$((n+1))
  done < $E/repl.txt
  [ $n -ge 1 ] || fail "no replacements recorded"
  [ -d $E/bucket4.gone ] && mv $E/bucket4.gone $E/s/bucket4
  snapshot > $E/after.txt
  [ -z "$(comm -23 $E/before.txt $E/after.txt)" ] || { comm -23 $E/before.txt $E/after.txt; fail "old objects changed"; }
  echo "   $n replacement(s) restore byte-identical, no c4 shard; $(wc -l < $E/before.txt) old objects unchanged"
}

for NAT in rclone native; do
  for SCENARIO in readable gone; do
    echo "== crypt=$NAT scenario=$SCENARIO"
    for MODE in nocopy copy; do
      setup $MODE $SCENARIO $NAT
      migrate $MODE
      D=$(field downloaded); U=$(field uploaded); S=$(field server_side); R=$(field readback); H=$(hashed)
      echo "   $MODE: relocated downloaded=$D uploaded=$U server_side=$S readback=$R hash_verified=$H"
      grep "relocated: downloaded" $E/run.log | head -3 | sed 's/^/     /'
      if [ $MODE = copy ]; then
        grep -q "server-side by the provider" $E/plan.txt || { cat $E/plan.txt; fail "plan has no server-side note"; }
        grep "server-side by the provider" $E/plan.txt | sed 's/^ */   plan: /'
        [ "$S" -gt 0 ] || fail "nothing was copied server-side"
        [ "$H" -gt 0 ] || fail "no shard was hash-verified"
        # Phase 1 moved every shard through the PC; now well under half.
        [ $((2 * (D + U))) -lt $((BEFORE_D + BEFORE_U)) ] || fail "transfer not reduced: $D+$U vs $BEFORE_D+$BEFORE_U"
        [ "$R" -lt "$BEFORE_R" ] || fail "readback not reduced: $R vs $BEFORE_R (hash verification unused)"
        echo "   PASS: through the PC $((D + U)) bytes (was $((BEFORE_D + BEFORE_U))), readback $R (was $BEFORE_R)"
      else
        [ "$S" -eq 0 ] || fail "server-side copy used with RPOOL_MIGRATE_NO_COPY"
        BEFORE_D=$D; BEFORE_U=$U; BEFORE_R=$R
      fi
      check_result
    done
  done
done
echo "SERVER COPY E2E OK"
