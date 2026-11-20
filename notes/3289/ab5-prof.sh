#!/usr/bin/env bash
# The 9d stack against main before it, per function: release compilers of main 9cb569834 (m) and
# of #3256's top ff566fe4f (for n = the yield-order-fix worktree's std), each row built with -g,
# cachegrind at 1 thread; then the functions whose counts differ by >= 100k, m vs n.
set -uo pipefail
REPO=/Users/andii/dev/velt/branches/yield-order-fix
W=$HOME/ab-stack
mkdir -p "$W/tmp" "$W/prof"
export TMPDIR=$W/tmp CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
build() { # tree rev
  local v=$1 rev=$2
  if [[ ! -x $W/tc-$v/velt ]]; then
    rm -rf "$W/$v-src"; mkdir -p "$W/$v-src"
    git -C "$REPO" archive "$rev" | tar -x -C "$W/$v-src"
    (cd "$W/$v-src" && CARGO_TARGET_DIR=$W/target-$v cargo build --release -q -p veltc -p velt_rt) || { echo "build FAIL $v"; exit 1; }
    mkdir -p "$W/tc-$v"; cp "$W/target-$v/release/velt" "$W/target-$v/release/libvelt_rt.a" "$W/tc-$v/"; rm -rf "$W/target-$v"
  fi
}
build m 9cb569834
build b ff566fe4f
rm -rf "$W/n-src"; cp -a "$W/b-src" "$W/n-src"
(cd "$REPO" && git diff --name-only ff566fe4f -- std) | while read -r f; do
  if [[ -e $REPO/$f ]]; then mkdir -p "$(dirname "$W/n-src/$f")"; cp "$REPO/$f" "$W/n-src/$f"; else rm -f "$W/n-src/$f"; fi
done
(cd "$REPO" && git ls-files --others --exclude-standard -- std) | while read -r f; do mkdir -p "$(dirname "$W/n-src/$f")"; cp "$REPO/$f" "$W/n-src/$f"; done
cd "$W/prof"
for r in "$@"; do
  for v in m n; do
    tc=$v; [ $v = n ] && tc=b
    exe=$W/prof/$r-$v
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$W/tc-$tc/libvelt_rt.a "$W/tc-$tc/velt" build --release -g --backend llvm "$W/$v-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "build FAIL $r-$v"; continue; }
    VELT_THREADS=1 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file="$exe.cg" "$exe" < /dev/null > /dev/null 2>&1
    echo "$r-$v total $(cg_annotate --auto=no "$exe.cg" 2>/dev/null | grep -m1 'PROGRAM TOTALS' | awk '{print $1}')"
    cg_annotate --auto=no "$exe.cg" 2>/dev/null | sed -n '/Function:file summary/,$p' | grep -E '^[<>] ' |
      awk '{gsub(",","",$2); n=$2; f=$NF; sub(":$","",f); sub(/:\/.*/,"",f); a[f]+=n} END {for (f in a) print f, a[f]}' | sort > "fn-$r-$v.txt"
  done
  echo "### $r (m = main 9cb569834, n = #3289 top)"
  join -a1 -a2 -e0 -o 0,1.2,2.2 "fn-$r-m.txt" "fn-$r-n.txt" |
    awk '{d=$3-$2; a=d<0?-d:d; if (a>=100000) printf "%14d %14d %+12d  %s\n", $2, $3, d, $1}' | sort -k3 -n
done
echo "== stack prof end"
