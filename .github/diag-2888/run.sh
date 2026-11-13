#!/usr/bin/env bash
# Temporary diagnostic for #2888 (closed unmerged): the take-back program on a GitHub Windows
# runner, at several sleeps, with the rescue counts. Run from the tree's root; $1 names the tree.
set -u
tree=$1
diag=$2
out=diag-out
mkdir -p $out
for ms in 300 600 1200 2400; do
  sed "s/sleep(300)/sleep($ms)/" "$diag/relax.velt" > $out/relax_$ms.velt
  VELT_SCHEDULER=std target/debug/velt build $out/relax_$ms.velt -o $out/relax_$ms.exe || exit 1
done
echo "### $tree: 1 ms sleeps"
start=$(date +%s%N)
cat > $out/sl.velt <<'V'
async function main() {
  for (let i = 0; i < 50; i++) {
    await sleep(1);
  }
  console.log("done");
}
V
VELT_SCHEDULER=std target/debug/velt build $out/sl.velt -o $out/sl.exe && $out/sl.exe
echo "50 x sleep(1): $(( ($(date +%s%N) - start) / 1000000 )) ms (with the build)"
s2=$(date +%s%N); $out/sl.exe >/dev/null; echo "50 x sleep(1), run only: $(( ($(date +%s%N) - s2) / 1000000 )) ms"
nproc 2>/dev/null; echo "NUMBER_OF_PROCESSORS=${NUMBER_OF_PROCESSORS:-?}"
for ms in 300 600 1200 2400; do
  ok=0
  for i in ${RUNS:-1 2 3 4 5 6 7 8 9 10}; do
    s=$(date +%s%N)
    o=$(VELT_THREADS=1 VELT_RESCUE=1 VELT_RESCUE_STATS=1 $out/relax_$ms.exe 2>&1)
    t=$(( ($(date +%s%N) - s) / 1000000 ))
    case "$o" in *"worker 0 true"*) ok=$((ok+1)); r=back;; *) r=LATE;; esac
    echo "$tree sleep=$ms run=$i $r ${t}ms :: $(echo "$o" | tr '\n' '|')"
  done
  echo "### $tree sleep=$ms: taken back $ok/10"
done
