#!/bin/bash
# Runs a probe under lldb until its ICE and prints the backtrace of the panicking thread.
#   bash bt.sh <exe> [tries=8]
cd "$(dirname "$0")" || exit 1
exe=$1; tries=${2:-8}
for i in $(seq "$tries"); do
  VELT_THREADS=4 VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 lldb -b \
    -o "breakpoint set -n velt_rt_panic" \
    -o "run" -o "bt 40" -o "kill" -o "quit" "$exe" > bt.out 2>&1
  if grep -q "frame #1" bt.out; then
    grep -E "frame #[0-9]+" bt.out | sed -E 's/0x[0-9a-f]+ //; s/ at .*//' | cut -c1-150 | head -40
    exit 0
  fi
done
echo "no ICE in $tries tries"; grep -c "Breakpoint 1" bt.out
