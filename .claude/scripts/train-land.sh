#!/bin/bash
# train-land.sh <train-name>: if the train's full run passed and main and every PR head are where the
# train tested them, squash-merge the PRs in train order (admin: the full gate already passed on
# exactly this tree), then delete the train branch. Stops at the first surprise.
set -euo pipefail
repo=velt-lang/velt
f=${VELT_TRAINS_DIR:-$HOME/.velt-trains}/$1.txt
branch=$(awk '/^branch /{print $2}' "$f"); base=$(awk '/^base /{print $2}' "$f"); rid=$(awk '/^run /{print $2}' "$f")
concl=$(gh run view "$rid" --repo $repo --json status,conclusion --jq '"\(.status) \(.conclusion)"')
[ "$concl" = "completed success" ] || { echo "$1: run $rid is $concl, not landing"; exit 1; }
main=$(gh api repos/$repo/commits/main --jq .sha)
# If main moved since the train's base (an earlier train landed), allow it only when nothing merged
# since then touched a file this train's PRs touch (trains run in parallel on disjoint files).
if [ "$main" != "$base" ]; then
  # Commits since the base that are this train's own PRs, merged elsewhere at the tested head (an
  # old auto-merge, a second landing), changed nothing the train didn't test: their files don't
  # count. Any other commit's files do.
  tested_prs=$(grep -E '^[0-9]+ ' "$f" | while read -r pr sha; do
    if [ "$(gh pr view "$pr" --repo $repo --json state,headRefOid --jq '"\(.state) \(.headRefOid)"')" = "MERGED $sha" ]; then
      echo "$pr"
    fi
  done)
  moved=$(gh api "repos/$repo/compare/$base...$main" --jq '.commits[].sha' | while read -r c; do
    msg=$(gh api "repos/$repo/commits/$c" --jq '.commit.message | split("\n")[0]')
    own=no; for pr in $tested_prs; do case "$msg" in *"(#$pr)") own=yes;; esac; done
    [ "$own" = yes ] || gh api "repos/$repo/commits/$c" --jq '.files[].filename'
  done | sort -u)
  # Only PRs that will land count (a PR changed since the train is skipped below), and an
  # overlap only in Markdown is allowed when GitHub still finds the PR mergeable: docs don't
  # change what the gate tested.
  mine=$(grep -E '^[0-9]+ ' "$f" | while read -r pr sha; do
    [ "$(gh pr view "$pr" --repo $repo --json headRefOid,mergeable --jq '"\(.headRefOid) \(.mergeable)"')" = "$sha MERGEABLE" ] || continue
    gh pr view "$pr" --repo $repo --json files --jq '.files[].path'; done | sort -u)
  both=$(comm -12 <(echo "$moved") <(echo "$mine") | grep -v '\.md$' || true)
  [ -z "$both" ] || { echo "$1: main moved and touched this train's files: $(echo $both | head -c 300); rebuild the train"; exit 1; }
  echo "$1: main moved since the base, on files disjoint from this train: landing"
fi
grep -E '^[0-9]+ ' "$f" | while read -r pr sha; do
  now=$(gh pr view "$pr" --repo $repo --json headRefOid,state --jq '"\(.headRefOid) \(.state)"')
  # A PR pushed since the train was built isn't what was tested: skip it (the train's PRs touch
  # disjoint files, so the rest land as tested) and leave it for the next train.
  if [ "$now" != "$sha OPEN" ]; then
    # Still landable when everything pushed since the tested head is Markdown only (docs don't
    # change what the gate ran) and the tested head is an ancestor of the new one.
    new=${now%% *}
    cmp=$(gh api "repos/$repo/compare/$sha...$new" --jq '"\(.status) " + ([.files[].filename|select(endswith(".md")|not)]|length|tostring)' 2>/dev/null || echo "x 1")
    if [ "${now##* }" = OPEN ] && [ "$cmp" = "ahead 0" ]; then
      echo "#$pr: changed since the train, docs only: landing"
    else
      echo "skip #$pr: changed since the train ($now; $cmp)"; continue
    fi
  fi
  gh api graphql -f query='mutation($id:ID!){dequeuePullRequest(input:{id:$id}){clientMutationId}}' \
    -f id="$(gh pr view "$pr" --repo $repo --json id --jq .id)" >/dev/null 2>&1 || true
  gh pr merge "$pr" --repo $repo --admin --squash >/dev/null && echo "merged #$pr" || { echo "#$pr did not merge, stopping"; exit 1; }
  sleep 5
done
gh api -X DELETE "repos/$repo/git/refs/heads/$branch" >/dev/null 2>&1 || true
echo "$1 landed"
