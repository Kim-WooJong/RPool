#!/bin/sh
# Pool change migration phase 2 against real cloud accounts: the same checks
# as scripts/linux-docker/migrate-server-copy-e2e.sh, but on crypt remotes
# from the user's own rclone config instead of a local WebDAV server.
#
#   RPOOL=<rpool binary> REMOTES="a_crypt b_crypt c_crypt d_crypt" sh $0
#
# The first three remotes stay in the pool (their backends should support
# server-side copy); the fourth leaves it. Everything is written under a fresh
# `rpool-e2e-<run>/` folder on each remote and that folder is purged at the
# end. The rclone config is only read, never changed; RPool's own config is a
# temporary directory. Top-level entries of each remote are compared before
# and after (not printed) to prove nothing was written outside the folder.
#   SCENARIO=readable: the fourth remote is still readable -> shards streamed.
#   SCENARIO=gone:     its folder is purged first -> shards rebuilt.
# Each scenario runs with RPOOL_MIGRATE_NO_COPY=1 (phase 1) and with copies.
#
# Progress: every step prints a timestamped line, and a heartbeat every
# HEARTBEAT seconds (default 30) shows the running step, its elapsed time and
# the last line of its log. A step running longer than STEP_WARN seconds
# (default 600) is flagged "SLOW" so a stall is visible while it runs.
set -eu
fail() { echo "FAIL: $*"; exit 1; }
: "${RPOOL:?set RPOOL to the rpool binary}"
: "${REMOTES:?set REMOTES to four crypt remote names}"
set -- $REMOTES
[ $# -eq 4 ] || fail "REMOTES needs exactly four names"
C1=$1 C2=$2 C3=$3 C4=$4
BASE=${TMPDIR:-/tmp}/rpool-cloud-e2e.$$
RUN=rpool-e2e-$(date +%Y%m%d%H%M%S)-$$
mkdir -p $BASE
TOUCHED=""
HEARTBEAT=${HEARTBEAT:-30} STEP_WARN=${STEP_WARN:-600}
T0=$(date +%s)
now() { date +%s; }
# step NAME LOG: announce a step; the heartbeat reports it until the next one.
step() {
  echo "$1" > $BASE/step; echo "${2:-}" > $BASE/steplog; now > $BASE/stepstart
  echo "[$(date +%H:%M:%S) +$(( $(now) - T0 ))s] $1"
}
heartbeat() {
  while sleep $HEARTBEAT; do
    [ -f $BASE/step ] || continue
    t=$(( $(now) - $(cat $BASE/stepstart) )); l=$(cat $BASE/steplog)
    last=""; [ -n "$l" ] && [ -f "$l" ] && last=$(grep -v "not found - using defaults" "$l" | tail -1 | cut -c1-110)
    flag=""; [ $t -gt $STEP_WARN ] && flag=" SLOW (over ${STEP_WARN}s)"
    echo "    ... $(cat $BASE/step): ${t}s${flag}${last:+ | $last}"
  done
}
# rclone purge fails on some crypt backends; fall back to delete + rmdirs.
rm_tree() { rclone purge "$1" >/dev/null 2>&1 || { rclone delete "$1" >/dev/null 2>&1; rclone rmdirs "$1" >/dev/null 2>&1; } || true; }

# Test folders (rpool-e2e-*) of this or a concurrent run are excluded.
top() { rclone lsf --max-depth 1 "$1:" 2>/dev/null | grep -v "^rpool-e2e-" | sort | shasum | cut -c1-16; }
for r in $C1 $C2 $C3 $C4; do eval "TOP_$(echo $r | tr -c 'A-Za-z0-9\n' _)=$(top $r)"; done

cleanup() {
  [ -n "${HB:-}" ] && kill $HB 2>/dev/null
  echo "[$(date +%H:%M:%S) +$(( $(now) - T0 ))s] cleanup: removing $RUN/"
  for p in $(printf '%s\n' $TOUCHED | sort -u); do
    rm_tree "$p"
    r=${p%%:*}
    rclone lsf --max-depth 1 "$r:" 2>/dev/null | grep -q "^$RUN/\$" && echo "WARN: $p was not fully removed"
  done
  rm -rf $BASE
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Listing of every object (path, size) under the four run folders.
snapshot() {
  for r in $C1 $C2 $C3 $C4; do
    rclone lsf -R --files-only --format ps "$r:$ROOT" 2>/dev/null | sed "s|^|$r:|"
  done | sort
}
field() { sed -n "s/.*\[relocate\].* $1=\([0-9]*\).*/\1/p" $E/run.log | awk '{s+=$1} END {print s+0}'; }
hashed() { sed -n 's/.*\[relocate\].* hash_verified=\([0-9]*\)\/.*/\1/p' $E/run.log | awk '{s+=$1} END {print s+0}'; }

setup() { # MODE SCENARIO NATIVE
  ROOT=$RUN/$3-$2-$1
  for r in $C1 $C2 $C3 $C4; do TOUCHED="$TOUCHED $r:$RUN"; done
  E=$BASE/$3-$2-$1; mkdir -p $E/cfg $E/src
  export RPOOL_CONFIG_DIR=$E/cfg RPOOL_PC_ID=PC-A
  NATIVE=""; [ $3 = native ] && NATIVE=--native-crypt
  step "$ROOT: pool set (4 remotes)"
  $RPOOL pool set shared --remote $C1:$ROOT --remote $C2:$ROOT --remote $C3:$ROOT --remote $C4:$ROOT \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null
  for n in 1 2 3; do
    python3 -c "import random,sys; random.seed($n); sys.stdout.buffer.write(random.randbytes($((1200000 + n * 900000))))" > $E/src/f$n.bin
    step "$ROOT: put f$n.bin ($(wc -c < $E/src/f$n.bin | tr -d ' ') bytes)" $E/put$n.log
    RCLONE_LOG_FILE=$E/rclone-put.log RCLONE_LOG_LEVEL=ERROR $RPOOL put $E/src/f$n.bin --pool shared > $E/put$n.log 2>&1 \
      || { cat $E/put$n.log; tail -20 $E/rclone-put.log 2>/dev/null; fail put; }
  done
  step "$ROOT: list objects before"
  snapshot > $E/before.txt
  $RPOOL pool set shared --remote $C1:$ROOT --remote $C2:$ROOT --remote $C3:$ROOT \
    --data-shards 2 --parity-shards 1 --shard-mib 1 $NATIVE >/dev/null 2>&1
  if [ $2 = gone ]; then
    step "$ROOT: remove $C4 data (scenario gone)"
    rm_tree "$C4:$ROOT"
    grep -v "^$C4:" $E/before.txt > $E/before.kept || true
    mv $E/before.kept $E/before.txt
  fi
  return 0
}

migrate() { # MODE
  step "$ROOT: migrate plan" $E/plan.txt
  $RPOOL pool migrate plan shared > $E/plan.txt 2>&1 || { cat $E/plan.txt; fail plan; }
  ID=$(sed -n 's/.*migration[ _]id[=: ]*\([0-9a-f]\{32\}\).*/\1/p' $E/plan.txt | head -1)
  [ -n "$ID" ] || { cat $E/plan.txt; fail "no migration id"; }
  step "$ROOT: migrate run" $E/run.log
  # rclone errors (level ERROR only) go to a file shown on failure.
  export RCLONE_LOG_FILE=$E/rclone-run.log RCLONE_LOG_LEVEL=ERROR
  if [ $1 = nocopy ]; then
    RPOOL_MIGRATE_NO_COPY=1 $RPOOL pool migrate run shared --id $ID > $E/run.log 2>&1 || { tail -30 $E/run.log; tail -20 $E/rclone-run.log 2>/dev/null; fail run; }
  else
    $RPOOL pool migrate run shared --id $ID > $E/run.log 2>&1 || { tail -30 $E/run.log; tail -20 $E/rclone-run.log 2>/dev/null; fail run; }
  fi
  unset RCLONE_LOG_FILE RCLONE_LOG_LEVEL
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
  step "$ROOT: restore and compare" $E/get.log
  n=0
  while read new name; do
    $RPOOL get "$new" $E/restored.bin > $E/get.log 2>&1 || { cat $E/get.log; fail "restore $new"; }
    cmp $E/restored.bin $E/src/$name || fail "$name differs after migration"
    rclone cat "$new" | grep -q "\"$C4:" && fail "$name still references $C4"
    n=$((n+1))
  done < $E/repl.txt
  [ $n -ge 1 ] || fail "no replacements recorded"
  step "$ROOT: list objects after"
  snapshot > $E/after.txt
  [ -z "$(comm -23 $E/before.txt $E/after.txt)" ] || { comm -23 $E/before.txt $E/after.txt; fail "old objects changed"; }
  echo "   $n replacement(s) restore byte-identical, no $C4 shard; $(wc -l < $E/before.txt | tr -d ' ') old objects unchanged"
}

echo "remotes kept: $C1 $C2 $C3; leaving: $C4; folder: $RUN"
echo "runs: crypt=${CRYPTS:-rclone native} scenario=${SCENARIOS:-readable gone} x no-copy/copy; heartbeat ${HEARTBEAT}s, slow step ${STEP_WARN}s"
heartbeat & HB=$!
K=0
for NAT in ${CRYPTS:-rclone native}; do
  for SCENARIO in ${SCENARIOS:-readable gone}; do
    echo "== crypt=$NAT scenario=$SCENARIO"
    for MODE in nocopy copy; do
      K=$((K+1)); echo "-- run $K: $NAT $SCENARIO $MODE"
      setup $MODE $SCENARIO $NAT
      migrate $MODE
      D=$(field downloaded); U=$(field uploaded); S=$(field server_side); R=$(field readback); H=$(hashed)
      echo "   $MODE: relocated downloaded=$D uploaded=$U server_side=$S readback=$R hash_verified=$H"
      if [ $MODE = copy ]; then
        grep "server-side by the provider" $E/plan.txt | sed 's/^ */   plan: /' || true
        [ "$S" -gt 0 ] || fail "nothing was copied server-side"
        [ $((2 * (D + U))) -lt $((BEFORE_D + BEFORE_U)) ] || fail "transfer not reduced: $D+$U vs $BEFORE_D+$BEFORE_U"
        echo "   PASS: through the PC $((D + U)) bytes (was $((BEFORE_D + BEFORE_U))), readback $R (was $BEFORE_R), hash-verified $H"
      else
        [ "$S" -eq 0 ] || fail "server-side copy used with RPOOL_MIGRATE_NO_COPY"
        BEFORE_D=$D; BEFORE_U=$U; BEFORE_R=$R
      fi
      check_result
    done
  done
done

step "check top level of every remote"
for r in $C1 $C2 $C3 $C4; do
  v=TOP_$(echo $r | tr -c 'A-Za-z0-9\n' _)
  [ "$(eval echo \$$v)" = "$(top $r)" ] || fail "$r: top level changed outside $RUN/"
done
echo "PASS: nothing written outside $RUN/ on any remote"
echo "CLOUD SERVER COPY E2E OK in $(( $(now) - T0 ))s"
