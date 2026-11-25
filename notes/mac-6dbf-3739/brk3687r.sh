#!/bin/bash
# #3687 PR2 break checks (round 2 of #3739): each edits the head's std in a copy, then runs the
# root form of the golden, and the token test where it says so. Also the root form on PR1's std.
# Every run has a timeout and writes to a file.
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
V="${VELT_BIN:-$HOME/dev/velt/branches/mac6dbf-target/debug/velt}"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_RT_LIB=$HOME/dev/velt/branches/mac6dbf-target/debug/libvelt_rt.a
cd "$W" || exit 1
G=tests/golden/std/promise_combinators_order_root
check() { # <title> <std>
  VELT_STD="$2" VELT_RT_DEBUG_ALLOC=1 VELT_THREADS=1 python3 "$S/tmo.py" 120 "$V" run $G.velt > "$S/b.o" 2> "$S/b.e"
  echo "$1: exit $? $(diff -q "$S/b.o" $G.out >/dev/null && echo '= .out (golden passes)' || echo 'golden differs (bites)')"
}
check "PR1's std (the root form must fail there)" "$S/pr1/std"
brk() { # <title> <file in std> <old> <new> [test]
  local O="$S/brk3687r"
  rm -rf "$O" && mkdir -p "$O" && cp -R std "$O/std"
  python3 "$S/swap.py" "$O/std/$2" "$3" "$4" > /dev/null || echo "SWAP FAILED: $1"
  check "break: $1" "$O/std"
  if [ "${5:-}" = test ]; then
    VELT_STD="$O/std" VELT_THREADS=1 python3 "$S/tmo.py" 180 "$V" test "$O/std/lang/hooks/sched_yield_token.test.velt" > "$S/bt.o" 2>&1
    echo "  token test: $(grep -E 'test result' "$S/bt.o" | tail -1)"
  fi
  rm -rf "$O"
}
brk "the holdback stays for yield turns" lang/hooks/sched_fire.velt "    if (!yieldTurn && m.load32(k + t.T_STATE) === t.YIELDED) {" "    if (m.load32(k + t.T_STATE) === t.YIELDED) {"
brk "a child's second yield in a poll shares the first's turn" lang/hooks/sched_yield.velt "function sharedRootYield(k: usize): usize {
  if (m.load32Relaxed(k + t.T_STATE) === t.YIELDED) {
    return rootYieldAgain(k);
  }" "function sharedRootYield(k: usize): usize {
  if (m.load32Relaxed(k + t.T_STATE) === t.YIELDED) {
    return ls.POLL;
  }"
brk "a root yield behind listed yields has no record" lang/hooks/sched_yield.velt "    return rootRecordFirst(k, b);" "    return ls.POLL;"
brk "a started promise's ask of POLL is always ready" lang/hooks/sched_yield_list.velt "  if (k === 0 || x === 0 || m.load32(x + t.T_STATE) !== t.YIELDED) {" "  if (true) {" test
brk "no turn for a started promise's ask of POLL" lang/hooks/sched_yield_list.velt "  otherTurn(k, w);
  return 0;" "  return 0;" test
brk "that turn behind the yields made so far" lang/hooks/sched_yield_list.velt "  join(k, record(m.wakerClone(w), m.ld(b + B_ROOT)));" "  join(k, record(m.wakerClone(w), nextStamp(b)));" test
brk "no YIELDING flag for that turn (neither checked nor set)" lang/hooks/sched_yield_list.velt "  if (isNode) {
    if ((m.load32(w + node.N_FLAGS) & node.YIELDING) !== 0) {
      // Its turn is on the list.
      return 0;
    }
    m.or32(w + node.N_FLAGS, node.YIELDING);
  }" "" test
brk "never park" lang/hooks/sched_yield_list.velt "    m.st(k + t.T_PARKED, b);
    m.st(k + t.T_YIELDS, 0);" "" test
brk "un-parking re-stamps a turn placed before parking" lang/hooks/sched_yield_list.velt "  if (m.load32(k + t.T_STATE) === t.YIELDED && !t.has(k, t.ROOT_ALONE)) {" "  if (m.load32(k + t.T_STATE) === t.YIELDED) {" test
brk "a poll's start keeps the last poll's placed turn" lang/hooks/sched_run.velt "      t.flags(k, 0, t.ROOT_ALONE);
      if ((fl & t.YIELDS) !== 0) {" "      if ((fl & t.YIELDS) !== 0) {" test
