#!/bin/bash
# count.sh with every program in one codegen unit (the 2-unit split's effect).
export VELT_CODEGEN_UNITS=1
exec bash "$(dirname "$0")/count.sh" "$@"
