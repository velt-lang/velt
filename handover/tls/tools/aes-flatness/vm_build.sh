#!/usr/bin/env bash
# Builds base (origin/main) and head (worktree HEAD + worktree std) compilers in the VM.
set -euo pipefail
source ~/.cargo/env
W=$HOME/aes2417
WT=${WT:?set WT to a velt worktree}
mkdir -p $W
cd $WT
for side in base head; do
  ref=origin/main; [ $side = head ] && ref=HEAD
  rm -rf $W/$side; mkdir -p $W/$side
  git archive $ref | tar -x -C $W/$side
  find $W/$side -type f -exec touch {} +
  echo "$side: $(git rev-parse $ref)"
done
rm -rf $W/head/std && cp -R $WT/std $W/head/std
for side in base head; do
  (cd $W/$side && CARGO_TARGET_DIR=$W/target-$side cargo build --release -q -p veltc -p velt_rt)
  echo "built $side"
done
echo DONE
