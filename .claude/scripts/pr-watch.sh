#!/usr/bin/env bash
# Prints one line per event on velt-lang/velt's open pull requests and merge queue, for a
# maintainer session to react to (docs/internals/maintaining.md "Watching for events"). In Claude
# Code, run it as a Monitor; it ends after MINUTES (default 29) so the session re-arms it.
#   NEW PR #n | PUSH #n | READY #n | COMMENT #n | MERGED #n | QUEUE FAIL #n run <id>
set -u
repo=velt-lang/velt
end=$(( $(date +%s) + ${MINUTES:-29} * 60 ))
since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
seen_merged=""; seen_failed=""; seen_comments=""
declare -A heads drafts
open_prs() {
  gh api "repos/$repo/pulls?state=open&per_page=100" --jq '.[]|"\(.number) \(.head.sha) \(.draft)"'
}
while read -r n h dr; do heads[$n]=$h; drafts[$n]=$dr; done < <(open_prs 2>/dev/null)
while [ "$(date +%s)" -lt "$end" ]; do
  cur=$(open_prs 2>/dev/null) || { sleep 60; continue; }
  while read -r n h dr; do
    [ -z "$n" ] && continue
    if [ -z "${heads[$n]:-}" ]; then echo "NEW PR #$n"
    elif [ "${heads[$n]}" != "$h" ]; then echo "PUSH #$n"; fi
    if [ "${drafts[$n]:-}" = "true" ] && [ "$dr" = "false" ]; then echo "READY #$n"; fi
    heads[$n]=$h; drafts[$n]=$dr
  done <<< "$cur"
  for c in $(gh api "repos/$repo/issues/comments?since=$since&per_page=100" \
      --jq '.[]|"\(.id):\(.issue_url|split("/")|last)"' 2>/dev/null); do
    n=${c#*:}; [ -z "${heads[$n]:-}" ] && continue
    case " $seen_comments " in *" ${c%%:*} "*) ;;
      *) echo "COMMENT #$n"; seen_comments="$seen_comments ${c%%:*}";; esac
  done
  for m in $(gh api "repos/$repo/pulls?state=closed&sort=updated&direction=desc&per_page=20" \
      --jq ".[]|select(.merged_at!=null and .merged_at>\"$since\")|.number" 2>/dev/null); do
    case " $seen_merged " in *" $m "*) ;; *) echo "MERGED #$m"; seen_merged="$seen_merged $m";; esac
  done
  for f in $(gh run list --repo "$repo" --event merge_group --limit 10 \
      --json databaseId,conclusion,updatedAt,headBranch \
      --jq ".[]|select(.updatedAt>\"$since\" and (.conclusion==\"failure\" or .conclusion==\"cancelled\"))|\"\(.databaseId):\(.headBranch|capture(\"pr-(?<n>[0-9]+)\").n)\"" 2>/dev/null); do
    case " $seen_failed " in *" $f "*) ;;
      *) echo "QUEUE FAIL #${f#*:} run ${f%%:*}"; seen_failed="$seen_failed $f";; esac
  done
  sleep 90
done
