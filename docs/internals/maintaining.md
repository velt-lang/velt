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
   Never copy JavaScript's bug sources (`undefined`, implicit coercion, `any`): one way of
   doing things, enforced by the compiler with a precise error and fix-it, even when ported
   code must change. Where valid TypeScript depends on a JavaScript behavior everywhere,
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
origin/main often. Run scripts/check.sh (the checks your changes need) while iterating and before
each pull request; the merge queue runs the whole gate. One
pull request per issue or a few related ones ("Closes #N") with tests and docs; a pull request
that builds on another of yours joins it in a stack (gh stack). Follow-ups outside scope become
focused issues. Never stop processes you didn't start.
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
  `ci:full` selects everything). The **merge queue** runs the whole gate against the pull request
  merged with the latest `main`, then squash-merges: two jobs per OS, Linux always, Windows and
  macOS when the change touches OS-specific code (`crates/xtask/src/os.rs`). Enable it with `gh pr merge <n>
  --auto` once the review is approved.
- Before queueing, strip any generated footer from the description: it becomes the commit message.
- Queue a stack bottom-up. When the bottom pull request merges, the next one is retargeted to
  `main` and runs its checks again.
- A nightly workflow runs the whole gate with PostgreSQL and Redis, the Cranelift memory stress
  run (`bench/compile/stress.sh`, `stress.ps1`: long_main_16000 under 2 GB) on Linux and Windows,
  and the `tsc` oracle for `velt check --ts-compat` (`crates/velt_tscompat/tests/oracle.rs`
  against the `typescript` pinned in `tests/tscompat-oracle`), and the benchmark guard
  (`bench/nightly.sh`: the instructions each benchmark executes, counted with valgrind, against
  the last run that passed; more than 3% more fails and opens the issue "nightly benchmarks
  regressed"); fix failures first. An intended slowdown is accepted with `gh workflow run nightly
  -f only=bench -f bench_baseline=accept`. Run one part alone with `gh workflow run nightly -f
  only=stress` (or `gate`, `oracle`, `bench`).
- Each push to `main` (the `main` jobs in `ci.yml`) runs the whole gate on Windows and macOS,
  which covers the changes the queue checked on Linux only, and opens or updates the issue
  "main fails on <OS>" when that fails; fix it first. The same jobs refresh the build cache on
  all three OSes: pull request and merge queue runs restore it but never save their own.
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
- **Platform gaps** found by CI (e.g. a test that is skipped on one OS) get an issue, and any CI
  workaround references it and is removed with the fix.
- **Disk**: builds are large. On small system disks put `CARGO_TARGET_DIR`, `VELT_GOLDEN_WORK`
  (and ideally `TMP`, `RUSTUP_HOME`, `CARGO_HOME`) on a bigger disk, and remove worktrees and
  their build directories when a pull request has merged. In WSL, build only under the mounted
  bigger disk (`/mnt/d/...`, `TMPDIR` too): the WSL home directory lives in a disk image on the
  system drive that grows and never shrinks on its own.

## Starting a maintainer session

Read this file, CLAUDE.md and CONTRIBUTING.md, then:
1. `gh pr list` — review what is open; queue what is approved and green.
2. `gh run list --workflow nightly` and the queue — fix anything red on `main` first.
3. `gh issue list --milestone …` — check running batches, hand out the next ones.
4. Prune merged branches and stale worktrees.
