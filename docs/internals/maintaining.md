# Maintaining Velt

How the project is run day to day: planning work, reviewing and merging pull requests, keeping
`main` green and fast. Written for maintainers, human or AI. Contributors only need
[CONTRIBUTING.md](../../CONTRIBUTING.md).

## Principles (decide with these)

1. **A modern TypeScript without the funky stuff inherited from JavaScript.** Look exactly like
   TypeScript where TypeScript already solved a problem, so TypeScript developers are productive
   at once and valid TypeScript runs as it does in Node. But Velt is a native backend language,
   not a way to compile to JavaScript: TypeScript is the starting point, not the limit. Add
   Velt-only features wherever they make programs substantially faster (integer widths,
   `shared`, `extend`, and more to come), as opt-ins layered on the TypeScript-compatible core.
   Never copy JavaScript's bug sources in typed code (implicit coercion): one way
   of doing things, enforced by the compiler with a precise error and fix-it, even when ported
   code must change. `any` is accepted for TypeScript compatibility and checked at run time,
   like C#'s `dynamic`: a wrong type is a `TypeError` where the value enters typed code, never
   undefined behaviour. Where valid TypeScript depends on a JavaScript behavior everywhere,
   TypeScript compatibility comes first and Velt matches Node exactly: numbers and strings are
   conditions with JavaScript's truthiness (`0`, `NaN` and `""` are falsy, `n || 5` returns an
   operand). No backward compatibility before 1.0.
2. **Performance first.** Rust-level speed and memory, no garbage collector, no pauses. A change
   that regresses a benchmark beyond ±3% doesn't merge; JS-like semantics are paid for only
   where a program actually uses them (e.g. reference counts only for types a program shares).
3. **Safety.** No memory unsafety reachable from ordinary Velt code: a misuse is a compile error
   or a clear runtime error, never undefined behaviour. The debug runtime's checking allocator
   and the goldens enforce it; FFI and native packages get the same bar (signature checks,
   checksums before anything is loaded).
4. **The repository is public.** No secrets, personal data, machine paths or internal process
   wording in code, docs, issues, PR descriptions or commits.

Design decisions and their reasons live in [design/](design/); the language reference says what
is implemented and marks the rest **Planned**.

## Planning

- Work is tracked as **GitHub issues** grouped by milestone. Issues cover the language, the
  standard library and the tooling, not projects built on top of Velt.
- Hand out work in **batches** of related issues that touch the same area, and avoid running two
  batches that restructure the same crates at once (sema, lowering, the runtime and std are the
  usual hot spots; a large cross-cutting change gets the area to itself).
- Design issues get a short design as an issue comment before implementation; the maintainer
  reviews it there.
- Every agent or person works in its **own git worktree** (`velt/main` stays on `main`,
  `velt/branches/<task>` per task) and its own build directory.
- Start every batch with a **fresh agent** and a self-contained prompt (the template below, plus
  where builds and temporary files go on the machine). Send more work to an existing agent only
  for short review fixes on its own pull request: a long-running agent carries its whole history
  into every step, which costs far more than restating the context.
- Work that builds on another open pull request becomes a **stack** (`gh stack`, CONTRIBUTING.md
  "Commits and pull requests"); independent work stays in separate pull requests.

Prompt template for an agent batch:
```
Clone https://github.com/velt-lang/velt (or work in your worktree), create a branch named after
the batch, read CLAUDE.md and CONTRIBUTING.md, then work through issues <list> in order (design
issues: post a short design as a comment first). Keep changes focused on your issues and merge
origin/main often. Run `cargo xtask install-hooks` once (a pre-push hook that runs
`cargo xtask precheck`: fmt, velt fmt, file sizes and clippy on the changed files), and run
scripts/check.sh (the checks your changes need) while iterating and before each pull request;
the merge queue runs the whole gate. Never push with --no-verify to get past a failing precheck. One
pull request per issue or a few related ones ("Closes #N") with tests and docs; a pull request
that builds on another of yours joins it in a stack (gh stack), and so does work past about 1,500
changed lines. Before opening each pull request, run `cargo xtask precheck` and go through the
self-check in CONTRIBUTING.md "Before opening a pull request" (Node comparison for new goldens,
docs, a regression test per fix, no unwrap/expect on user input, benchmark numbers for hot paths,
tests ordered by awaits, not sleeps); post its result as the first comment on the pull request,
not in the description. When a review ends in `Verdict: CHANGES NEEDED`, push fixes for every
required item and say in a comment which commit addresses which item. Follow-ups outside scope
become focused issues. Never stop processes you didn't start.
```

## Reviewing a pull request

Check, in this order, and post the result as a review (`gh pr review <n> --comment --body-file
<file>`, or `--approve` / `--request-changes` on someone else's pull request): a summary, then
numbered required changes with file:line and the fix, then follow-ups that may become issues, and
as the **last line** exactly one of

```
Verdict: APPROVE
Verdict: CHANGES NEEDED
```

That line is what the maintainer loop reads to know where a pull request stands, so write it in
this plain form, with nothing else on the line, and only in reviews; it judges the head the review
was posted on. Older reviews wrote it in bold (`**Verdict: APPROVE**`); the status command below
still reads those. A pull request without the author's self-check comment (CONTRIBUTING.md "Before
opening a pull request") gets it as a required change.

1. **Hygiene**: commits authored by the repository's configured author, no `Co-authored-by` or
   tool trailers; the description has no generated footers or session links (it becomes the
   commit message); no internal process wording. The `hygiene` check covers the mechanical part
   (`cargo xtask pr-lint`); review still reads for stale or process wording it can't recognize.
2. **Correctness against the issue and the design**: JavaScript-identical results where the
   program is valid TypeScript (compare with Node); regression tests for every fix.
3. **Soundness and security**: ownership across FFI, use-after-free paths (stale handles must be
   checked registry keys), path traversal, unverified downloads, panics across `extern "C"`.
4. **Performance**: benchmark numbers for anything on a hot path, measured on a quiet machine,
   per platform when the change is platform-specific; nothing outside ±3%.
5. **Contracts**: changes to `ast.rs`, `hir`, `vir.rs` or `docs/internals/contracts/**` are
   deliberate, minimal and documented.
6. **Standards**: CLAUDE.md coding standards (file sizes, docs, no `unwrap` on user input),
   clippy clean on the pinned toolchain, docs updated for user-visible changes.

Large pull requests get a read-only review agent per pull request; a maintainer posts the
review.

**Re-review** after `CHANGES NEEDED`: look only at the commits pushed since the reviewed head
(`git log -p <reviewed>..<head>`; `git range-diff` when the branch was rebased) and at each
required item of the earlier review, not the whole pull request again. When every item is
addressed and the new commits raise nothing new, approve and queue in the same step:

```
gh pr review <n> --comment --body-file review.md   # ends in "Verdict: APPROVE"
gh pr merge <n> --auto
```

The rule stays that nothing is queued before an approving review; queueing with it removes the
wait between approval and the queue. A new finding in the new commits is a required change like
any other (`Verdict: CHANGES NEEDED`); an older nit that the first review missed is a follow-up,
not a reason for another round.

## Merging

- `main` only changes through pull requests. Required checks: `hygiene` (the title, description
  and commits: `cargo xtask pr-lint`, `.github/workflows/hygiene.yml`) and `ci` (on the pull
  request, the checks its changes need: `cargo xtask check`, rules in
  `crates/xtask/src/plan.rs`; the label `ci:full` selects everything). The **merge queue** runs the whole gate against the pull request
  merged with the latest `main`, then squash-merges: per OS a lint-and-test job and the goldens
  (built once, then checked in parallel shards: `.github/workflows/queue-goldens.yml`), Linux
  always, Windows and macOS when the change touches OS-specific code (`crates/xtask/src/os.rs`).
  For a change to WebAssembly code the same build also packs the WebAssembly goldens, which then
  run in shards of their own (`WASM_SHARDS` in `os.rs`: 4 on Windows, 2 on Linux and macOS)
  instead of in the lint-and-test job, where on Windows they took about 45 minutes. Enable it with `gh pr merge <n>
  --auto` in the same step as the approving review, never before it.
- The description becomes the commit message. `hygiene` fails on generated footers, trailers,
  session links, machine paths and review-process wording, so there is no footer to strip by hand
  any more; editing the description re-runs it. It does not judge whether the text still
  describes the head: that stays part of the review.
- Queue a stack bottom-up. When the bottom pull request merges, the next one is retargeted to
  `main` and runs its checks again.
- A nightly workflow runs the whole gate with PostgreSQL and Redis (and the parser's robustness
  test cutting every golden at every character, `VELT_ROBUSTNESS=full`, and the WebAssembly
  goldens, which pull requests and the queue run only for WebAssembly changes), the Cranelift memory
  stress run (`bench/compile/stress.sh`, `stress.ps1`: long_main_16000 under 2 GB) on Linux and
  Windows, the WebAssembly goldens on Windows and macOS (built once per OS, then run in shards,
  as in the queue), and the `tsc` oracle for
  `velt check --ts-compat` (`crates/velt_tscompat/tests/oracle.rs` against the `typescript`
  pinned in `tests/tscompat-oracle`), and the benchmark guard
  (`bench/nightly.sh`: the instructions each benchmark executes, counted with valgrind, against
  the last run that passed, `compile/check_all_std` per code line of `std/`; more than 3% more
  fails and opens, or comments on, the issue "nightly benchmarks regressed" with the benchmarks
  over the limit and the commits since the baseline; a second table in the job summary marks the
  benchmarks that take longer than their Node version, by wall time, which doesn't fail the
  job); fix failures first ("A red nightly" below).
  An intended slowdown is accepted with `gh workflow run nightly -f only=bench -f
  bench_baseline=accept`. Run one part alone with `gh workflow run nightly -f only=stress` (or
  `gate`, `oracle`, `bench`, `wasm`).
- A push to `main` (the `main` jobs in `ci.yml`) runs the whole gate on Windows and macOS when
  the queue checked the change on Linux only, and opens or updates the issue "main fails on
  <OS>" when that fails; fix it first. A commit the queue tested on every OS skips them, unless
  it touches the toolchain or the dependencies (`main-plan`: `cargo xtask affected --base
  HEAD^1`, the queue's own plan, since the queue tested the commit on its parent; rules in
  `crates/xtask/src/os.rs`). A commit no passing merge queue run tested (a direct push or an
  admin merge) runs every OS. The same jobs refresh the build cache: on Linux at every push, on
  Windows and macOS when they run. Pull request and merge queue runs restore it but never save
  their own.
- A check the selection missed shows up in the merge queue; tighten the rule in
  `crates/xtask/src/plan.rs` (with a test in `plan_tests.rs`) rather than adding `ci:full`
  habitually.

## Keeping the machinery healthy

- **Toolchain**: Rust is pinned in `rust-toolchain.toml`. Upgrade it in its own pull request
  (new versions bring new lints; clippy runs with `-D warnings`).
- **Flaky tests are bugs**: fix the cause instead of re-running. No wall-clock limits: a cost
  test counts the work (polls, buffer growths, bytes sent) or measures CPU time (a process's in
  `tests/common/process_work.rs`, compared between interleaved sizes; a thread's or a child
  process's against a generous bound), and a speed check belongs in `bench/`. Tests and goldens
  order events by awaits, channels or observed state, not by sleeps; a wait loop's deadline is a
  hang guard (a minute), not an assertion. Tests that build executables must not reuse a path a
  crashed process may still lock.
- **A red nightly** gets an answer the same day, so red doesn't become the normal state:
  - The bench guard holds its baseline until a run passes, so every later run fails again and
    the commit range in the issue grows. Measure before deciding: count the regressed
    benchmark's instructions at the range's ends and bisect it, with cachegrind as
    `bench/nightly.sh` does (`bench/compile/count_check.sh WORKDIR REF...` for
    `compile/check_all_std`, `bench/compare_refs.sh` for the programs; Linux only, so on macOS
    in a Linux container, where the count doesn't depend on load).
  - A commit that slows the compiler or a program without need gets a fix with a regression
    test. Growth that is the point of the change (a type-checking feature that costs work on
    every line, a slower but correct runtime path) is accepted with `gh workflow run nightly -f
    only=bench -f bench_baseline=accept`; say on the issue which commits it was and why, then
    close it. More of `std/` to check is not a regression: `compile/check_all_std` is compared
    per code line of `std/` (blank and comment lines don't count, so added documentation can't
    hide a slowdown); `bench/compile/all_std.vlt` imports every std module so that all those
    lines are checked (a test in `crates/veltc/tests/bench_programs.rs` names a missing one).
    The bench job runs on a fixed runner image: the counts depend on its C library and valgrind,
    so a new image is a pull request of its own, followed by accepting the new counts.
  - Any other job: find the commit (`gh run view <id> --log-failed`), fix it with a regression
    test or open an issue that says why it waits.
- **Platform gaps** found by CI (e.g. a test that is skipped on one OS) get an issue, and any CI
  workaround references it and is removed with the fix.
- **Disk**: builds are large. On small system disks put `CARGO_TARGET_DIR`, `VELT_GOLDEN_WORK`
  (and ideally `TMP`, `RUSTUP_HOME`, `CARGO_HOME`) on a bigger disk, and remove worktrees and
  their build directories when a pull request has merged. In WSL, build only under the mounted
  bigger disk (`/mnt/d/...`, `TMPDIR` too): the WSL home directory lives in a disk image on the
  system drive that grows and never shrinks on its own.

## The maintainer loop

Most of the time a pull request spends open is spent waiting for a review or a re-review, not
being fixed. So a maintainer session (`/maintain` in Claude Code, `.claude/commands/maintain.md`)
works in this order, finishing each step before the next:

1. **What is red** on `main`, in the merge queue or in the nightly workflow (`gh run list
   --branch main --workflow ci`, `gh run list --workflow nightly`, the issues "main fails on
   <OS>" and "nightly benchmarks regressed"): fix it or hand it out first.
2. **Re-reviews**: pull requests whose last verdict is `CHANGES NEEDED` and that have new commits
   since (above). They approve and queue in one step when everything is addressed.
3. **Unreviewed pull requests, smallest first**: small ones merge fastest and unblock stacks.
4. **Issues and batches**: `gh issue list --milestone …`; check running batches, hand out the
   next ones.
5. **Housekeeping**: prune merged branches, stale worktrees and their build directories.

Where each open pull request stands, smallest first (size, last verdict, and whether commits
arrived after it):

```
gh pr list --repo velt-lang/velt -L 100 \
  --json number,additions,deletions,headRefOid,isDraft,reviews --jq '.[]
  | select(.isDraft | not)
  | ([.reviews[] | select(.body | test("(?m)^\\**Verdict: "))] | last) as $r
  | [.number, .additions + .deletions,
     (($r.body // "") | capture("(?m)^\\**Verdict: (?<v>APPROVE|CHANGES NEEDED)").v // "unreviewed"),
     (if $r == null then "-" elif $r.commit.oid == .headRefOid then "head reviewed"
      else "new commits" end)]
  | @tsv' | sort -t$'\t' -k2 -n
```

`CHANGES NEEDED` with `new commits` is step 2; `unreviewed` is step 3; `APPROVE` with `new
commits` (a rebase, or a fix pushed after approval) gets a quick look at the new commits and is
queued again if the queue dropped it.

**Run it on a schedule** while batches are running, instead of only when someone starts it:
`/loop 30m /maintain` in Claude Code (every 30–60 minutes), or a scheduled agent with the same
prompt. Each run reports only what changed since the previous one. Stop the loop when no batch is
running and nothing is open.

### Watching for events

A loop every 30 minutes still leaves a reply waiting up to half an hour, and a missed author reply
once left a pull request waiting eleven hours. `.claude/scripts/pr-watch.sh` prints a line when
something happens: `NEW PR`, `PUSH`, `READY` (out of draft), `COMMENT`, `MERGED`, `QUEUE FAIL`.
Run it as a Monitor in Claude Code (it ends after 29 minutes; start it again each time) and act on
each line:

- `NEW PR`: start a review (smallest first when several arrive).
- `PUSH` then `COMMENT` from the author on a pull request at `CHANGES NEEDED`: the re-review is
  due now. Check who wrote each comment: a comment from the author, not only a push, can answer a
  review (a description-only change, or a question).
- `QUEUE FAIL`: triage it (below) before anything else.
- `MERGED`: queue the next pull request of a stack; remove the merged branch's worktree and build
  directory once its agent has finished.

### Reviewers and authors that keep their context

A review agent that reviewed a pull request is resumed for each later round of it (in Claude Code,
`SendMessage` to the same agent), not replaced by a new one: it still knows the code, its probes
and what it asked for, so a re-review of the required items takes minutes, not a fresh review's
hours. The same holds for the agent that wrote the pull request when it gets a review to fix.
Brief a re-review with what changed (the head, the author's reply) and a time box.

### What a review requires

A required change is one of: wrong output (against Node and `tsc --strict`), a crash or an `ICE`
on user input, unsoundness (memory, types, concurrency), a slowdown over 3% (the "±3%" rule
above), or a missing or ineffective test for the change (a test that passes with the code broken
on purpose counts as missing). Everything else (a pre-existing gap the review found, a design
alternative, an older nit) is a follow-up: the review lists it, and the maintainer files it as an
issue in the same step, so it neither blocks the pull request nor gets lost.

When a decision is the maintainer's to make (accept a documented difference, a partial fix with
the rest in an issue, which of two designs), decide with the recommendation, say on the pull
request or issue what was decided and why, and continue. Ask the owner only for what changes the
language or the project's direction.

### Triaging a merge queue failure

The queue runs with fail-fast, and each entry is tested merged with the entries ahead of it, so
one failure can cancel and fail the runs behind it. For each `QUEUE FAIL`:

1. Find the job that failed first: its annotation says "failed (see its failed step); cancelling
   the rest of the run"; the other failed jobs are cancellations.
2. Read its log (`gh run view <run> --job <job> --log`) and decide which pull request caused it:
   - **This pull request** (a test, the file-size rule, a conflict with what merged meanwhile):
     its author fixes it on the branch (merge `main`; split a file with headroom, not to exactly
     800 lines), and queues it again once CI is green. A mechanical fix needs no new review.
   - **A pull request ahead of it in the queue** (the failure is in code this one doesn't touch):
     take the culprit out of the queue so it stops failing the others, and leave the rest queued.
     A branch can't be pushed while it is queued, so dequeue it before its author pushes
     (the id from `gh pr view <n> --json id`):

     ```
     gh api graphql -f query='mutation { dequeuePullRequest(input: {id: "<id>"})
       { clientMutationId } }'
     ```
   - **A flaky test** (passes on its own, fails under load or timing): open an issue for the
     cause ("Flaky tests are bugs" above) and queue the innocent pull request again; the fix is
     the issue's, not a reason to hold the pull request.
3. A failure that a run created before the culprit was dequeued can still arrive afterwards;
   check the queue (`gh api graphql` on `mergeQueue`) before acting on it twice.
