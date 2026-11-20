#!/bin/bash
# restack.sh: in branches/units-u1a, puts each branch of the units stack onto the rebased one
# below it, as a temporary rb-* branch (the real branches move only after their tests).
# Each line: new temp branch, the branch to move, its old base, its new base.
cd /Users/andii/dev/velt/branches/units-u1a || exit 2
while read -r new branch old onto; do
  [ -z "$new" ] && continue
  git checkout -q -B "$new" "$branch" || exit 2
  if ! git rebase -q --onto "$onto" "$old" 2>/tmp/claude-restack.err; then
    echo "CONFLICT on $new:"
    git diff --name-only --diff-filter=U
    exit 1
  fi
  echo "$new $(git rev-parse --short HEAD) ($(git rev-list --count "$onto"..HEAD) commits on $onto)"
done <<'EOF'
rb-unit-fns units-unit-fns units-readonly-scans origin/main
rb-std-stage units-std-stage units-unit-fns rb-unit-fns
rb-sums-1 units-sums-1 units-std-stage rb-std-stage
EOF
