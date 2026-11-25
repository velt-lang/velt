#!/bin/bash
# Break checks for the merged turns (sched_yield_turn.velt), the recordless root turn and the
# self-review fixes, on the probes, the root golden and the token test (std edited in a copy).
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
V="$HOME/dev/velt/branches/mac6dbf-target/debug/velt"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_RT_LIB=$HOME/dev/velt/branches/mac6dbf-target/debug/libvelt_rt.a
cd "$W" || exit 1
G=tests/golden/std/promise_combinators_order_root
brk() { # <title> <file in std/lang/hooks> <old> <new>
  local O="$S/brkm"
  rm -rf "$O" && mkdir -p "$O" && cp -R std "$O/std"
  python3 "$S/swap.py" "$O/std/lang/hooks/$2" "$3" "$4" > /dev/null || echo "SWAP FAILED: $1"
  local r=""
  for p in merge_edges listed_after; do
    VELT_STD="$O/std" VELT_THREADS=1 python3 "$S/tmo.py" 120 "$V" run "$S/probes/$p.velt" > "$S/b.o" 2>&1
    r="$r $p: $(diff -q "$S/b.o" "$S/probes/new_$p.out" > /dev/null && echo same || echo "DIFFERS $(diff "$S/b.o" "$S/probes/new_$p.out" | grep -c '^[<>]')")"
  done
  VELT_STD="$O/std" VELT_THREADS=1 python3 "$S/tmo.py" 120 "$V" run $G.velt > "$S/b.o" 2>&1
  r="$r golden: $(diff -q "$S/b.o" $G.out > /dev/null && echo passes || echo "DIFFERS $(diff "$S/b.o" $G.out | grep -c '^[<>]')")"
  VELT_STD="$O/std" VELT_THREADS=1 python3 "$S/tmo.py" 180 "$V" test "$O/std/lang/hooks/sched_yield_token.test.velt" > "$S/bt.o" 2>&1
  echo "break: $1:$r token: $(grep -E 'test result' "$S/bt.o" | tail -1)"
  rm -rf "$O"
}
brk "merge with work pending" sched_yield_turn.velt "    micro.pending(k) ||
    m.ld(k + t.T_UNHANDLED) !== 0 ||
    m.load(k + t.T_WOKEN) !== 0" "    false"
brk "merge before this turn's waiter went on" sched_yield_turn.velt "    m.ld(b + ls.B_TAKEN) === 0 ||" ""
brk "merge any held root record, not only the next" sched_yield_turn.velt "    m.ld(b + ls.B_NEXT) !== f ||" "    m.ld(b + ls.B_NEXT) === 0 ||"
brk "merge after a wake of the root in this turn" sched_yield_turn.velt "    m.load32(k + t.T_STATE) === t.NOTIFIED ||
" ""
brk "never merge" sched_yield_list.velt "  if (k === cur && w === k && tn.nextTurn(k, f)) {" "  if (false) {"
brk "no first turn for the root (its records fire, the poll's end polls it)" sched_fire.velt "    if (rt !== 0 && ls.stampOf(y) >= rt) {" "    if (false) {"
brk "the root's record made for every join again (not only another waker's)" sched_yield_list.velt "    m.ld(y + Y_WAKER) !== ROOT && t.has(k, t.ROOT_ALONE)" "    m.ld(b + B_LAST) === 0 && t.has(k, t.ROOT_ALONE)"
brk "the root's turn first even before a record of its stamp" sched_fire.velt "    if (rt !== 0 && ls.stampOf(y) >= rt) {" "    if (rt !== 0) {"
