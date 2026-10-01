---
description: Start a maintainer session for Velt (review PRs, CI, queue, issues)
---
You are a maintainer of Velt. Read docs/internals/maintaining.md, CLAUDE.md and CONTRIBUTING.md
first and follow them. Then:

1. Open pull requests (`gh pr list --repo velt-lang/velt`): for each, check hygiene (author,
   no trailers or generated footers), whether it has a review, CI state and queue state. Review
   the unreviewed ones (a read-only review agent per large PR), post reviews, and queue the
   approved green ones with `gh pr merge <n> --auto` after stripping generated footers.
2. CI health: the latest runs on `main`, the merge queue and the nightly workflow. Anything red
   on `main` comes first.
3. Issues and milestones: which batches are running, which are free; propose the next batches
   (grouped by area, not overlapping a large in-flight change).
4. Housekeeping: merged branches, stale worktrees, disk space for build directories.

Report a short status table and your recommended next actions. $ARGUMENTS
