#!/bin/bash
# gold-both-ab.sh <label> <prev>: gold-both.sh (lazy + units goldens, failing files against
# val-<prev>), then the check/build A/B (ab7.sh).
T=/Users/andii/dev/velt/branches/handovers/mac-e688-2943/tools
bash "$T/gold-both.sh" "$1" "$2"
bash "$T/ab7.sh"
