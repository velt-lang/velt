# Maintaining Velt

How the project is run day to day: planning work, reviewing and merging pull requests, keeping
`main` green and fast. Written for maintainers, human or AI. Contributors only need
[CONTRIBUTING.md](../../CONTRIBUTING.md).

## Principles (decide with these)

1. **A modern TypeScript without the funky stuff inherited from JavaScript.** Look exactly like
   TypeScript where TypeScript already solved a problem; introduce new syntax only where
   TypeScript can't express what we need. Never copy JavaScript's bug sources (`undefined`,
   implicit coercion, falsy numbers/strings, `any`): one way of doing things, enforced by the
   compiler with a precise error and fix-it, even when ported code must change. No backward
   compatibility before 1.0.
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

Prompt template for an agent batch:
```
Clone https://github.com/velt-lang/velt (or work in your worktree), create a branch named after
the batch, read CLAUDE.md and CONTRIBUTING.md, then work through issues <list> in order (design
issues: post a short design as a comment first). Keep changes focused on your issues and merge
origin/main often. Run scripts/check.sh (the checks your changes need) while iterating and before
each pull request; the merge queue runs the whole gate. One
pull request per issue or a few related ones ("Closes #N") with tests and docs. Follow-ups
outside scope become focused issues.
```

## Reviewing a pull request

Check, in this order, and post the result as a review (verdict first, then numbered required
changes with file:line and the fix, then follow-ups that may become issues):

1. **Hygiene**: commits authored by the repository's configured author, no `Co-authored-by` or
   tool trailers; the description has no generated footers or session links (it becomes the
   commit message); no internal process wording.
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

## Merging

- `main` only changes through pull requests. Required check: `ci` (on the pull request, the
  checks its changes need: `cargo xtask check`, rules in `crates/xtask/src/plan.rs`; the label
  `ci:full` selects everything). The **merge queue** runs the whole gate on Linux, Windows and
  macOS against the pull request merged with the latest `main`, then squash-merges. Enable it with `gh pr merge <n>
  --auto` once the review is approved.
- Before queueing, strip any generated footer from the description: it becomes the commit message.
- A nightly workflow runs the whole gate with PostgreSQL and Redis; fix failures first.
- Each push to `main` refreshes the build cache (the `cache` jobs in `ci.yml`) on all three OSes:
  pull request and merge queue runs restore it but never save their own.
- A check the selection missed shows up in the merge queue; tighten the rule in
  `crates/xtask/src/plan.rs` (with a test in `plan_tests.rs`) rather than adding `ci:full`
  habitually.

## Keeping the machinery healthy

- **Toolchain**: Rust is pinned in `rust-toolchain.toml`. Upgrade it in its own pull request
  (new versions bring new lints; clippy runs with `-D warnings`).
- **Flaky tests are bugs**: fix the cause (timing tests compare interleaved measurements; tests
  that build executables must not reuse a path a crashed process may still lock) instead of
  re-running.
- **Platform gaps** found by CI (e.g. a test that is skipped on one OS) get an issue, and any CI
  workaround references it and is removed with the fix.
- **Disk**: builds are large. On small system disks put `CARGO_TARGET_DIR`, `VELT_GOLDEN_WORK`
  (and ideally `TMP`, `RUSTUP_HOME`, `CARGO_HOME`) on a bigger disk, and remove worktrees and
  their build directories when a pull request has merged.

## Starting a maintainer session

Read this file, CLAUDE.md and CONTRIBUTING.md, then:
1. `gh pr list` — review what is open; queue what is approved and green.
2. `gh run list --workflow nightly` and the queue — fix anything red on `main` first.
3. `gh issue list --milestone …` — check running batches, hand out the next ones.
4. Prune merged branches and stale worktrees.
