#!/usr/bin/env bash
# Put a pull request back into the merge queue after its run failed for GitHub's infrastructure
# only (requeue.py decided so), at most twice per head commit, and say so on the pull request.
# Run by .github/workflows/requeue.yml; every input comes through the environment:
#
#   GH_TOKEN      reads the pull request and posts the comment (the workflow's token)
#   QUEUE_TOKEN   enqueues it (the MERGE_QUEUE_TOKEN secret, or the workflow's token)
#   QUEUE_TOKEN_IS_SECRET  `true` when QUEUE_TOKEN is the secret
#   PR            the pull request's number
#   WHY           what happened, in words ("a runner was never acquired")
#   RUN_URL       the failed run
#   RUN_CREATED   when the failed run was created (ISO 8601, UTC)
#   GITHUB_REPOSITORY
#   DRY_RUN       `1`: print the comment and the enqueue instead of doing them (to try it on a run)
set -euo pipefail

MAX=2
owner=${GITHUB_REPOSITORY%/*}
name=${GITHUB_REPOSITORY#*/}

comment() {
    if [ "${DRY_RUN:-}" = 1 ]; then
        echo "would comment: $1"
        return
    fi
    gh api "repos/$GITHUB_REPOSITORY/issues/$PR/comments" -f body="$1" > /dev/null
}

pull_request() {
    gh api graphql -f owner="$owner" -f name="$name" -F pr="$PR" -f query='
      query($owner: String!, $name: String!, $pr: Int!) {
        repository(owner: $owner, name: $name) {
          pullRequest(number: $pr) { id state isInMergeQueue headRefOid }
        }
      }' --jq '.data.repository.pullRequest | [.state, .isInMergeQueue, .headRefOid, .id] | @tsv'
}

# The queue drops the entry when `ci` fails; give it a few minutes to do so.
until IFS=$'\t' read -r state queued head id <<< "$(pull_request)" &&
    { [ "$queued" = false ] || [ "$SECONDS" -ge 180 ]; }; do
    sleep 10
done
if [ "$state" != OPEN ]; then
    echo "#$PR is $state: nothing to do."
    exit 0
fi
if [ "$queued" != false ]; then
    # Queued again by hand, or still queued under a newer merge group (the failed run's branch
    # was replaced): either way it is in the queue.
    echo "#$PR is in the merge queue: nothing to do."
    exit 0
fi

# Only the commit the queue tested: a push after the run started is new code, which its author
# queues. The pull request's own `ci` run on its head was created before the queue's run.
pr_run=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs?head_sha=$head&event=pull_request&per_page=100" \
    --jq '[.workflow_runs[] | select(.name == "ci") | .created_at] | min // ""')
if [ -z "$pr_run" ] || [[ "$pr_run" > "$RUN_CREATED" ]]; then
    echo "#$PR's head $head is newer than the failed run ($RUN_CREATED; its ci run: ${pr_run:-none}): not re-queued."
    exit 0
fi

# At most MAX times per head commit, counted by a marker in the comments, so an outage that
# lasts can't keep a pull request cycling through the queue.
marker="<!-- requeue: $head -->"
count=$(gh api --paginate "repos/$GITHUB_REPOSITORY/issues/$PR/comments?per_page=100" \
    --jq "[.[] | select(.body | contains(\"$marker\"))] | length" | awk '{ s += $1 } END { print s + 0 }')
if [ "$count" -ge "$MAX" ]; then
    limit="<!-- requeue-limit: $head -->"
    seen=$(gh api --paginate "repos/$GITHUB_REPOSITORY/issues/$PR/comments?per_page=100" \
        --jq "[.[] | select(.body | contains(\"$limit\"))] | length" | awk '{ s += $1 } END { print s + 0 }')
    if [ "$seen" = 0 ]; then
        comment "The merge queue dropped this pull request again because $WHY ([run]($RUN_URL)). It was re-queued $MAX times for ${head:0:7} already, so it is left out: queue it again once the runners are back. $limit"
    fi
    echo "#$PR was re-queued $count times for $head: left out."
    exit 0
fi

if [ "${DRY_RUN:-}" = 1 ]; then
    echo "would enqueue #$PR ($id), re-queue $((count + 1)) of $MAX for $head"
    comment "Re-queued: the merge queue dropped this pull request because $WHY ([run]($RUN_URL)), not because of a check. Re-queue $((count + 1)) of $MAX for ${head:0:7}. $marker"
    exit 0
fi
err=$(mktemp)
if GH_TOKEN=$QUEUE_TOKEN gh api graphql -F id="$id" -f query='
      mutation($id: ID!) {
        enqueuePullRequest(input: { pullRequestId: $id }) { mergeQueueEntry { position } }
      }' > /dev/null 2> "$err"; then
    how=enqueuePullRequest
elif GH_TOKEN=$QUEUE_TOKEN gh pr merge "$PR" --repo "$GITHUB_REPOSITORY" --auto 2>> "$err"; then
    how="gh pr merge --auto"
else
    cat "$err"
    secret=
    [ "${QUEUE_TOKEN_IS_SECRET:-}" = true ] || secret=" (the workflow's token may not enqueue: the MERGE_QUEUE_TOKEN secret is not set)"
    comment "The merge queue dropped this pull request because $WHY ([run]($RUN_URL)), and re-queueing it failed$secret: queue it again by hand."
    exit 1
fi
echo "#$PR re-queued with $how ($((count + 1)) of $MAX for $head)."
comment "Re-queued: the merge queue dropped this pull request because $WHY ([run]($RUN_URL)), not because of a check. Re-queue $((count + 1)) of $MAX for ${head:0:7}. $marker"

# The workflow's own token starts no workflow runs for the events it causes (GitHub's rule for
# GITHUB_TOKEN; the docs list no exception for the merge queue): check that the queue's `ci` run
# for the entry started, and say so if not.
if [ "${QUEUE_TOKEN_IS_SECRET:-}" != true ]; then
    since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    for _ in $(seq 1 40); do
        sleep 15
        started=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs?event=merge_group&created=>=$since&per_page=100" \
            --jq "[.workflow_runs[] | select(.name == \"ci\" and (.head_branch | test(\"/pr-$PR-\")))] | length")
        if [ "$started" -gt 0 ]; then
            echo "The queue's ci run for #$PR started."
            exit 0
        fi
    done
    comment "No \`ci\` run started for the re-queued entry within 10 minutes: the workflow's token may not start merge queue checks (set the MERGE_QUEUE_TOKEN secret). If the queue is long, it may only be waiting its turn."
fi
