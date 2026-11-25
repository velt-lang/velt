#!/bin/bash
# The merge (ls.nextTurn), std only: goldens in 4 modes, the token and held tests, the probes vs
# Node, and the rows against PR1 and 18cc57d98's std (same compiler).
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
T="$HOME/dev/velt/branches/mac6dbf-target"
V="$T/debug/velt"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_RT_LIB=$HOME/dev/velt/branches/mac6dbf-target/debug/libvelt_rt.a VELT_STD="$W/std"
cd "$W" || exit 1
echo "######## goldens in 4 modes"
for g in promise_combinators_order promise_combinators_order_root catch_panic_yield_siblings; do
  G=tests/golden/std/$g
  for mode in debug --release; do
    for th in 1 4; do
      VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_THREADS=$th python3 "$S/tmo.py" 300 "$V" run $([ $mode = debug ] || echo --release) $G.velt > "$S/rc.o" 2> "$S/rc.e"
      echo "  $g $mode t$th: exit $? $(diff -q "$S/rc.o" $G.out > /dev/null && echo '= .out' || echo DIFF) $(grep -o 'blocks=[0-9/]*' "$S/rc.e" | tail -1)"
    done
  done
done
echo "######## token + held tests"
for f in sched_yield_token sched_yield_held; do
  for th in 1 4; do
    VELT_RT_DEBUG_ALLOC=1 VELT_THREADS=$th python3 "$S/tmo.py" 300 "$V" test std/lang/hooks/$f.test.velt > "$S/rt.o" 2>&1
    echo "  $f t$th: $(grep -E 'test result' "$S/rt.o" | tail -1)"
  done
done
echo "######## probes vs Node"
for p in merge_edges listed_after listed_root; do
  [ -f "$S/probes/node_$p.out" ] || bash "$S/probes/node_run.sh" $p > /dev/null
  grep -v -i "warning\|--trace" "$S/probes/node_$p.out" > "$S/probes/n_$p.out"
  for mode in debug --release; do
    for th in 1 4; do
      VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_THREADS=$th python3 "$S/tmo.py" 300 "$V" run $([ $mode = debug ] || echo --release) "$S/probes/$p.velt" > "$S/probes/v_$p.out" 2> "$S/rc.e"
      echo "  $p $mode t$th: exit $? $(diff -q "$S/probes/v_$p.out" "$S/probes/n_$p.out" > /dev/null && echo '= Node' || echo "DIFF $(diff "$S/probes/v_$p.out" "$S/probes/n_$p.out" | grep -c '^[<>]')") $(grep -o 'blocks=[0-9/]*' "$S/rc.e" | tail -1)"
    done
  done
done
echo "######## rows (median of 5): PR1, 18cc57d98's std, this std (same compiler for the last two)"
export VELT_RT_LIB="$T/release/libvelt_rt.a"
mkdir -p "$S/std18cc"
[ -d "$S/std18cc/std" ] || git archive 18cc57d98 std | tar -x -C "$S/std18cc"
A="$S/abm"
rm -rf "$A" && mkdir -p "$A"
instr() { VELT_THREADS=1 /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }'; }
median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }
for p in "$S/rows3687/inline_all_yields.velt" bench/async/root_yields_after_all.velt "$S/rows3687/noblock.velt" bench/async/race_yield.velt bench/async/fanout_all.velt bench/async/spawn_many.velt; do
  n=$(basename "$p" .velt)
  VELT_STD="$S/pr1/std" "$S/c3687r/velt-base" build --release "$p" -o "$A/$n-pr1" > /dev/null 2>&1 || echo "BUILD FAILED $n pr1"
  VELT_STD="$S/std18cc/std" "$V" build --release "$p" -o "$A/$n-old" > /dev/null 2>&1 || echo "BUILD FAILED $n old"
  "$V" build --release "$p" -o "$A/$n-new" > /dev/null 2>&1 || echo "BUILD FAILED $n new"
  a=""; b=""; c=""
  for k in 1 2 3 4 5; do a="$a $(instr "$A/$n-pr1")"; b="$b $(instr "$A/$n-old")"; c="$c $(instr "$A/$n-new")"; done
  am=$(echo $a | tr ' ' '\n' | median); bm=$(echo $b | tr ' ' '\n' | median); cm=$(echo $c | tr ' ' '\n' | median)
  awk -v p="$n" -v a="$am" -v b="$bm" -v c="$cm" 'BEGIN { printf "%-22s pr1 %11d 18cc %11d (%+.2f%%) new %11d (%+.2f%% vs pr1, %+.2f%% vs 18cc)\n", p, a, b, (b - a) * 100 / a, c, (c - a) * 100 / a, (c - b) * 100 / b }'
done
rm -rf "$A"
