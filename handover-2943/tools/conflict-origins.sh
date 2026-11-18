#!/bin/bash
# conflict-origins.sh: for each conflicted file of the merge in progress in the d1-open-pr
# worktree, the commits of origin/main since the merge base that touched it, marked "units"
# when the subject names #2943 (squash-merged units work the branch has in a later form) and
# "OTHER" otherwise (a change the merge must keep).
cd /Users/andii/dev/velt/branches/d1-open-pr || exit 1
base=$(git merge-base HEAD origin/main)
for f in $(git diff --name-only --diff-filter=U); do
  echo "== $f"
  git log --format='%h %s' "$base..origin/main" -- "$f" | while read -r line; do
    case "$line" in
      *"#2943"*) echo "  units  $line" ;;
      *) echo "  OTHER  $line" ;;
    esac
  done
done
