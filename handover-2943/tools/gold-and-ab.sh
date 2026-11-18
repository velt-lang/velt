#!/bin/bash
# gold-and-ab.sh <label>: the goldens built from std's unit (gold7.sh, val-<label>), the
# failing files against the previous run (failfiles.sh), then the check/build A/B (ab7.sh).
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/tools
prev=$2
bash "$T/gold7.sh" "$1"
bash "$T/failfiles.sh" "val-$prev" "val-$1"
bash "$T/ab7.sh"
