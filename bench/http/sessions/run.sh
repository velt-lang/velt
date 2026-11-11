#!/usr/bin/env bash
# #2730 prototype numbers: base (module lock retaken at every poll, one task per server with
# module state, #2748) against the prototype (connections spread, lock per stretch, marked
# objects). Usage:
#   BASE=<base worktree> PROTO=<proto worktree> BASE_VELT=<velt> PROTO_VELT=<velt> \
#   SERVER_CPUS=0,1,6,7,8,9,10,11 CLIENT_CPUS=2-5 OHA=<oha> bench/http/sessions/run.sh
# Prints: (a) instructions per request of /plaintext and /json (techempower server, 1 worker);
# (b) sessions server req/s (best of RUNS interleaved) at WORKERS workers and its instructions
# per request; (c) instructions per store of store.velt (flat object, object with 10 children).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/../lib.sh"
WORKERS="${WORKERS:-1 4 8}"
SIDES="base proto"
declare -A TREE=([base]="$BASE" [proto]="$PROTO")
declare -A VC=([base]="$BASE_VELT" [proto]="$PROTO_VELT")

for s in $SIDES; do
  t=${TREE[$s]}
  # The sessions bench lives on the prototype branch; the base tree gets a copy.
  mkdir -p "$t/bench/http/sessions"
  cp "$HERE/server.velt" "$HERE/package.velt" "$HERE/velt.lock.json" "$t/bench/http/sessions/" 2>/dev/null || true
  cp -r "$HERE/probes" "$t/bench/http/sessions/" 2>/dev/null || true
  VELT_STD="$t/std" "${VC[$s]}" build --release "$t/bench/http/techempower/server.velt" -o "$WORK/te-$s" >&2
  VELT_STD="$t/std" "${VC[$s]}" build --release "$t/bench/http/sessions/server.velt" -o "$WORK/se-$s" >&2
  VELT_STD="$t/std" "${VC[$s]}" build --release "$t/bench/http/sessions/probes/store.velt" -o "$WORK/st-$s" >&2
done

echo "| route | base Ir/req | proto Ir/req | change |"
echo "|---|---|---|---|"
for path in /plaintext /json; do
  declare -A IR
  for s in $SIDES; do
    PORT="$(random_port)"
    IR[$s]=$(ir_per_request "$path" 2000 12000 env VELT_THREADS=1 "$WORK/te-$s" "$PORT")
  done
  echo "| $path | ${IR[base]} | ${IR[proto]} | $(python3 -c "print('%+.2f%%' % (100.0*(${IR[proto]}-${IR[base]})/${IR[base]}))") |"
done

echo
echo "| workers | base req/s | proto req/s | base p99 | proto p99 | all runs base | all runs proto | load |"
echo "|---|---|---|---|---|---|---|---|"
for w in $WORKERS; do
  declare -A ALL BEST P99
  ALL=([base]="" [proto]="")
  BEST=([base]=0 [proto]=0)
  loads=""
  for r in $(seq 1 "$RUNS"); do
    loads="$loads $(load1)"
    for s in $SIDES; do
      PORT="$(random_port)"
      start_server /x env VELT_THREADS="$w" "$WORK/se-$s" "$PORT"
      pid="$STARTED_PID"
      warm "http://127.0.0.1:$PORT/x"
      read -r rps p99 ok < <(measure "http://127.0.0.1:$PORT/x")
      [ "$ok" = "1.0" ] || echo "warning: $s success rate $ok" >&2
      ALL[$s]="${ALL[$s]} $rps"
      if [ "$rps" -gt "${BEST[$s]}" ]; then BEST[$s]=$rps; P99[$s]=$p99; fi
      stop_server "$pid"
    done
  done
  echo "| $w | ${BEST[base]} | ${BEST[proto]} | ${P99[base]} ms | ${P99[proto]} ms |${ALL[base]} |${ALL[proto]} |$loads |"
done

echo
echo "| sessions Ir/req (1 worker) | base | proto |"
echo "|---|---|---|"
declare -A SIR
for s in $SIDES; do
  PORT="$(random_port)"
  SIR[$s]=$(ir_per_request /x 2000 12000 env VELT_THREADS=1 "$WORK/se-$s" "$PORT")
done
echo "| /x | ${SIR[base]} | ${SIR[proto]} |"

echo
echo "| store.velt mode | base Ir/store | proto Ir/store | marking cost per store |"
echo "|---|---|---|---|"
ir_of() {
  local out="$WORK/cgs.$RANDOM"
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$out" "$@" >/dev/null 2>&1
  awk '/^summary:/ {print $2}' "$out"
}
for mode in flat deep; do
  declare -A PS
  for s in $SIDES; do
    a=$(ir_of "$WORK/st-$s" "$mode" 20000)
    b=$(ir_of "$WORK/st-$s" "$mode" 120000)
    PS[$s]=$(( (b - a) / 100000 ))
  done
  echo "| $mode | ${PS[base]} | ${PS[proto]} | $(( PS[proto] - PS[base] )) |"
done
