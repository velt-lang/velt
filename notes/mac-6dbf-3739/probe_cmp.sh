#!/bin/bash
# probe_cmp.sh <probe>: Node's output vs this std and 18cc57d98's std (debug, 1 worker).
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
V="$HOME/dev/velt/branches/mac6dbf-target/debug/velt"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk
p="$1"
bash "$S/probes/node_run.sh" "$p" > /dev/null
grep -v -i "warning\|--trace" "$S/probes/node_$p.out" > "$S/probes/n_$p.out"
VELT_STD="$W/std" VELT_THREADS=1 python3 "$S/tmo.py" 120 "$V" run "$S/probes/$p.velt" > "$S/probes/new_$p.out" 2>&1
VELT_STD="$S/std18cc/std" VELT_THREADS=1 python3 "$S/tmo.py" 120 "$V" run "$S/probes/$p.velt" > "$S/probes/old_$p.out" 2>&1
cd "$S/probes" || exit 1
echo "new vs node: $(diff "new_$p.out" "n_$p.out" | grep -c '^[<>]') lines; 18cc vs node: $(diff "old_$p.out" "n_$p.out" | grep -c '^[<>]') lines; new vs 18cc: $(diff "new_$p.out" "old_$p.out" | grep -c '^[<>]') lines"
diff "new_$p.out" "n_$p.out"
