#!/bin/bash
# Usage: whyfail.sh <run-id>: print the failing step and the FAIL/error lines of each failed or cancelled job.
here=$(cd "$(dirname "$0")" && pwd); cd "$(git -C "$here" rev-parse --show-toplevel)"
run=$1
gh api "repos/velt-lang/velt/actions/runs/$run/jobs?per_page=100" --jq '.jobs[]|select(.conclusion=="failure" or .conclusion=="cancelled")|"\(.id)\t\(.name)"' | while IFS=$'\t' read -r id name; do
  out=$(gh api --allow-escape-sequences "repos/velt-lang/velt/actions/jobs/$id/logs" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -E "^\S+ +(FAIL|error(\[|:))|FAILED:|panicked at|##\[error\]" | grep -v "Process completed with exit code" | head -6 | cut -c30-230)
  [ -n "$out" ] && { echo "== $name"; echo "$out"; }
done
