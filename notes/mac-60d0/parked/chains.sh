#!/bin/bash
# chains.velt with the #3274 std and its `nested` mutant (std-nested, made by breaks.sh), 1 and 4
# workers, counted: output and block balance side by side.
cd "$(dirname "$0")" || exit 1
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
for label in fix nested; do
  std=/Users/andii/dev/velt/branches/parked-task/std; [ $label = nested ] && std=$PWD/std-nested
  VELT_STD=$std VELT_SCHEDULER=std "$V" build chains.velt -o "./chains.$label" > "chains.$label.build" 2>&1 || { echo "$label: build failed"; head -8 "chains.$label.build"; continue; }
  for th in 1 4; do
    VELT_THREADS=$th VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1 "./chains.$label" > "chains.$label.out" 2> "chains.$label.err"
    echo "== $label T$th exit $? $(grep -o 'blocks=[0-9]*/[0-9]*' "chains.$label.err") $(grep -o 'ICE[^|]*' "chains.$label.err" | head -1)"
    tr '\n' '|' < "chains.$label.out"; echo
  done
done
