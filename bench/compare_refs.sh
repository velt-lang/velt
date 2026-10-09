#!/usr/bin/env bash
# Instruction counts of two git refs, compared with bench/nightly.sh (Linux):
#
#   bench/compare_refs.sh BASE_REF HEAD_REF WORK_DIR [RUNS]
#
# Exports each ref with `git archive` into WORK_DIR/{base,head}, runs bench/nightly.sh once in
# the base tree, then RUNS times (default 2) in the head tree with `--baseline` set to the base
# counts, so a difference that shows in every run is the change, not noise. Each tree builds in
# its own target directory under WORK_DIR. The Markdown tables are WORK_DIR/head-run<N>.md, the
# counts WORK_DIR/{base,head-run<N>}.tsv. Run it from inside the repository.
set -euo pipefail
if [ $# -lt 3 ]; then
  echo "usage: bench/compare_refs.sh BASE_REF HEAD_REF WORK_DIR [RUNS]" >&2
  exit 2
fi
BASE_REF=$1
HEAD_REF=$2
WORK=$3
RUNS=${4:-2}
REPO=$(git rev-parse --show-toplevel)
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)

for side in base head; do
  ref=$BASE_REF
  [ "$side" = head ] && ref=$HEAD_REF
  rm -rf "${WORK:?}/$side"
  mkdir -p "$WORK/$side"
  git -C "$REPO" archive "$ref" | tar -x -C "$WORK/$side"
  echo "$side: $(git -C "$REPO" rev-parse "$ref")" >&2
done

CARGO_TARGET_DIR="$WORK/target-base" "$WORK/base/bench/nightly.sh" --out "$WORK/base.tsv" \
  > "$WORK/base.md"
status=0
for run in $(seq 1 "$RUNS"); do
  CARGO_TARGET_DIR="$WORK/target-head" "$WORK/head/bench/nightly.sh" \
    --baseline "$WORK/base.tsv" --out "$WORK/head-run$run.tsv" > "$WORK/head-run$run.md" \
    || status=$?
  echo "== head run $run (exit $status)"
  cat "$WORK/head-run$run.md"
done
exit "$status"
