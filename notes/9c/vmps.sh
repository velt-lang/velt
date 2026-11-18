#!/bin/bash
# My processes in the VM that belong to the 9c measurements (prb.sh, its cargo builds).
pgrep -af "prb.sh|b9cb" | grep -v pgrep || echo "none"
