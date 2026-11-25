#!/bin/bash
# #3739 on main + #3706 (branches/root-turns-final): build, the three goldens in 4 modes, the token
# and held tests, golden sets, std tests, eager_std_identity, std_lint, precheck.
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-final"
T="$HOME/dev/velt/branches/mac6dbf-target"
export CARGO_TARGET_DIR="$T" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_STD="$W/std"
V="$T/debug/velt"
cd "$W" || exit 1
echo "######## build"
rm -rf "$T/debug/targets" "$T/release/targets"
cargo build -p veltc --bin velt 2>&1 | grep -E "^(error|warning)|Finished" | head -5
touch crates/velt_rt/src/lib.rs
cargo build -p velt_rt 2>&1 | grep -E "^error|Finished" | head -2
cargo build --release -p velt_rt 2>&1 | grep -E "^error|Finished" | head -2
echo "######## the three goldens in 4 modes"
for g in promise_combinators_order promise_combinators_order_root catch_panic_yield_siblings; do
  G=tests/golden/std/$g
  for mode in debug release; do
    if [ $mode = debug ]; then L="$T/debug/libvelt_rt.a"; F=""; else L="$T/release/libvelt_rt.a"; F="--release"; fi
    for th in 1 4; do
      VELT_RT_LIB="$L" VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_THREADS=$th python3 "$S/tmo.py" 300 "$V" run $F $G.velt > "$S/rc.o" 2> "$S/rc.e"
      echo "  $g $mode t$th: exit $? $(diff -q "$S/rc.o" $G.out > /dev/null && echo '= .out' || echo DIFF) $(grep -o 'blocks=[0-9/]*' "$S/rc.e" | tail -1)"
    done
  done
done
echo "######## token + held tests"
for f in sched_yield_token sched_yield_held; do
  for th in 1 4; do
    VELT_RT_LIB="$T/debug/libvelt_rt.a" VELT_RT_DEBUG_ALLOC=1 VELT_THREADS=$th python3 "$S/tmo.py" 300 "$V" test std/lang/hooks/$f.test.velt > "$S/rt.o" 2>&1
    echo "  $f t$th: $(grep -E 'test result' "$S/rt.o" | tail -1)"
  done
done
echo "######## goldens (sets)"
for set in promise_combinators catch_panic yield async_ promise_ spawn all_ await top_level race sched timer; do
  VELT_GOLDEN=$set python3 "$S/tmo.py" 1500 cargo test --workspace --test golden > "$S/gs.o" 2>&1
  echo "--- $set: $(grep -E 'test golden \.\.\.|test result' "$S/gs.o" | tail -1)"
  grep -E "FAILED|mismatch|^---- " "$S/gs.o" | head -5
done
echo "######## std tests (release, 4 workers)"
for d in std/lang/hooks std/task std/sync; do
  VELT_RT_LIB="$T/release/libvelt_rt.a" VELT_THREADS=4 python3 "$S/tmo.py" 900 "$V" test --release $d > "$S/rt.o" 2>&1
  echo "  $d: $(grep -E 'test result' "$S/rt.o" | tail -1)"
done
for t in eager_std_identity std_lint; do
  python3 "$S/tmo.py" 1800 cargo test --workspace --test $t > "$S/t.o" 2>&1
  echo "######## $t: exit $? $(grep -E 'test result' "$S/t.o" | tail -1)"
done
echo "######## velt_sema lazy_std + velt_vir"
cargo test -p velt_sema --lib lazy_std 2>&1 | grep -E "test result" | tail -1
cargo test -p velt_vir 2>&1 | grep -E "test result|FAILED|panicked" | tail -2
echo "######## precheck"
python3 "$S/tmo.py" 1200 cargo xtask precheck > "$S/pre.o" 2>&1; echo "  exit $?"; grep -E "lines besides|FAILED|passed" "$S/pre.o" | tail -3
rm -rf "$T/debug/incremental" "$T/release/incremental"
echo "######## done"
