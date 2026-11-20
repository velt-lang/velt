#!/bin/bash
# push-u1.sh: in branches/units-u1a, moves each U1 PR branch to its rebased rb-* head and
# force-pushes it with a lease on the head the remote has now.
cd /Users/andii/dev/velt/branches/units-u1a || exit 2
git fetch -q origin || exit 2
while read -r branch rb; do
  [ -z "$branch" ] && continue
  remote=$(git rev-parse "origin/$branch") || exit 2
  git branch -f "$branch" "$rb" || exit 2
  if git push --no-verify -q --force-with-lease="$branch:$remote" origin "$branch"; then
    echo "$branch: $(git rev-parse --short "$remote") -> $(git rev-parse --short "$branch")"
  else
    echo "PUSH FAILED: $branch"
    exit 1
  fi
done <<'EOF'
units-codec rb-codec
units-codec-hir rb-hir
units-codec-defs rb-defs
units-deftable rb-deftable
units-codec-tables rb-tables
units-codec-ctx rb-ctx
EOF
