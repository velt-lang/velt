---
description: Start a maintainer session for Velt (review PRs, CI, queue, issues)
---
You are a maintainer of Velt. Read docs/internals/maintaining.md (above all "The maintainer
loop" and "Reviewing a pull request"), CLAUDE.md and CONTRIBUTING.md first and follow them. Then
work in this order, finishing each step before the next:

1. **Red first.** The latest runs on `main` (`gh run list --branch main --workflow ci`), the merge
   queue, the nightly workflow, and open "main fails on <OS>" or "nightly benchmarks regressed"
   issues. Fix or dispatch anything red there before reviewing.
2. **Re-reviews.** List the open pull requests with their last verdict (the command in
   maintaining.md "The maintainer loop"). Those at `CHANGES NEEDED` with new commits since that
   review come next: review only the new commits and the required items of the earlier review.
   When every required item is addressed and nothing new is wrong, post the review ending in
   `Verdict: APPROVE` and run `gh pr merge <n> --auto` in the same step; otherwise post
   `Verdict: CHANGES NEEDED` with what is still open.
3. **Unreviewed pull requests, smallest first** (added plus deleted lines). Review each (a
   read-only review agent per large pull request) and post the review; its last line is exactly
   `Verdict: APPROVE` or `Verdict: CHANGES NEEDED`. Queue an approved one with
   `gh pr merge <n> --auto` in the same step. Nothing is queued without an approving review.
4. **Issues and batches.** Which batches are running and which are free; propose or start the
   next ones (grouped by area, not overlapping a large in-flight change).
5. **Housekeeping.** Merged branches, stale worktrees, disk space for build directories.

Report a short status table (pull request, size, verdict, CI, queue) and your recommended next
actions. When this runs on a schedule (`/loop 30m /maintain`), report only what changed since the
previous run. $ARGUMENTS
