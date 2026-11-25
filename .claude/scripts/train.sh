#!/bin/bash
# train.sh <pr> <pr> ...: build a merge train (approved PRs merged together onto main), push it as a
# `train-<mmdd-hhmm>` branch and start the full gate on it (workflow_dispatch of ci: every OS).
# Land it with train-land.sh once green; trains.sh watches several and does that for you.
#
# State: $VELT_TRAINS_DIR (default ~/.velt-trains) gets <name>.txt with `branch`, `base`, one
# "<pr> <head sha>" line per PR, and `run <id>`.
# Where the merge is built: in a temporary worktree of this repository, or, with
# VELT_TRAIN_SSH="<ssh args> user@host" (and VELT_TRAIN_REMOTE_REPO, default ~/velt/main), on that
# machine (useful where checking out the tree is slow, e.g. Windows).
set -euo pipefail
repo=velt-lang/velt
dir=${VELT_TRAINS_DIR:-$HOME/.velt-trains}; mkdir -p "$dir"
name="train-$(date -u +%m%d-%H%M)"
heads=""
for pr in "$@"; do
  h=$(gh pr view "$pr" --repo $repo --json headRefOid,mergeable,isDraft --jq 'select(.isDraft==false)|"\(.headRefOid) \(.mergeable)"')
  [ -n "$h" ] || { echo "#$pr is a draft or missing"; exit 1; }
  set -- $h
  [ "${2:-}" = CONFLICTING ] && { echo "#$pr conflicts with main: rebase it first"; exit 1; }
  heads="$heads $pr:$1"
done
# The merge script: run in a checkout of velt-lang/velt (local or remote), prints BASE <sha> on success.
build=$(cat <<EOF
set -e
git fetch -q origin main
W=\$(mktemp -d)/$name; git worktree add -q --detach "\$W" origin/main; cd "\$W"
base=\$(git rev-parse HEAD)
for ph in $heads; do
  pr=\${ph%%:*}; sha=\${ph#*:}
  git fetch -q origin pull/\$pr/head
  [ "\$(git rev-parse FETCH_HEAD)" = "\$sha" ] || { echo "MOVED #\$pr"; cd - >/dev/null; git worktree remove --force "\$W"; exit 1; }
  git -c core.hooksPath=/dev/null merge -q --no-ff --no-edit -m "train: #\$pr" \$sha >/dev/null 2>&1 || { echo "CONFLICT #\$pr"; git merge --abort; cd - >/dev/null; git worktree remove --force "\$W"; exit 1; }
done
git push -q origin HEAD:refs/heads/$name
echo "BASE \$base"
cd - >/dev/null; git worktree remove --force "\$W"
EOF
)
if [ -n "${VELT_TRAIN_SSH:-}" ]; then
  # shellcheck disable=SC2086
  out=$(echo "cd ${VELT_TRAIN_REMOTE_REPO:-~/velt/main} && $build" | ssh $VELT_TRAIN_SSH "bash -s" || true)
else
  out=$(cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)" && bash -c "$build" || true)
fi
echo "$out"
base=$(echo "$out" | awk '/^BASE /{print $2}')
[ -n "$base" ] || { echo "train not built: $(echo "$out" | grep -E "CONFLICT|MOVED" | tail -1)"; exit 1; }
{ echo "branch $name"; echo "base $base"; for ph in $heads; do echo "${ph%%:*} ${ph#*:}"; done; } > "$dir/$name.txt"
gh workflow run ci --repo $repo --ref "$name" >/dev/null
# The run takes a moment to appear; wait for it rather than guessing.
rid=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
  rid=$(gh run list --repo $repo --workflow ci --branch "$name" --limit 1 --json databaseId --jq '.[0].databaseId // empty')
  [ -n "$rid" ] && break
  sleep 3
done
[ -n "$rid" ] || { echo "$name: pushed, but its ci run didn't appear; add 'run <id>' to $dir/$name.txt by hand"; exit 1; }
echo "run $rid" >> "$dir/$name.txt"
echo "$name: run $rid for$heads"
