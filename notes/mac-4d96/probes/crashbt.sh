#!/bin/bash
# Runs a release probe under lldb until it crashes; prints every thread's backtrace.
#   bash crashbt.sh <exe> [tries=20]
cd "$(dirname "$0")" || exit 1
exe=$1; tries=${2:-20}
for i in $(seq "$tries"); do
  VELT_THREADS=4 lldb -b -o "run" -o "thread backtrace all" -o "register read x0 x1 x19 x20" -o "kill" -o "quit" "$exe" > crashbt.out 2>&1
  if grep -q "stop reason = EXC_BAD_ACCESS" crashbt.out; then
    grep -E "frame #[0-9]+|thread #|stop reason|x[0-9]+ = " crashbt.out | sed -E 's/_V3stdP4langP5hooksP[0-9]+//; s/ at .*//' | grep -v "pthread\|thread_start" | cut -c1-170 | head -70
    exit 0
  fi
done
echo "no crash in $tries tries"
