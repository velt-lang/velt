# Velt — TypeScript syntax, native code, Rust-level performance

Compiler written in Rust: `.vlt` → AST → HIR (typed, ownership-checked) → VIR (MIR-like) →
Cranelift (debug, JIT) or LLVM (release) → object → system linker + `velt_rt` (Rust staticlib
runtime; tokio-based async). Architecture: `docs/internals/README.md`. Language:
`docs/reference/`. Interfaces between crates: `docs/internals/contracts/README.md`.

Build, test tiers, coding standards and the language-change process are in `CONTRIBUTING.md`;
follow it. The essentials for agents:

## Commands
- Build everything: `cargo build --workspace`
- Unit tests for one crate: `cargo test -p <crate>`
- End-to-end tests: `cargo test --workspace --test golden` (filter: `VELT_GOLDEN=m1/strings`;
  `--workspace` resolves features like the gate's build, where `-p veltc` rebuilds crates)
- Documentation tests: `cargo test -p veltc --test docs`
- Lint: `cargo clippy -p <crate> --all-targets -- -D warnings`
- Quality gate: `scripts/check.sh` on macOS and Linux, `pwsh scripts/check.ps1` on Windows. It
  runs the checks your changes need, selected from the files changed since `origin/main`
  (`cargo xtask affected` prints the plan and why). Run it while iterating and before you report.
  The whole gate is `scripts/check-all.sh` / `check-all.ps1`; the merge queue and `main` run it on
  three OSes, so you don't need to (changes to the build, toolchain, CI or scripts make `check.sh`
  select everything anyway). Install cargo-nextest so only the selected tests run, in parallel.
  Filter goldens with `VELT_GOLDEN=<substring>` while working on one area.
- Before pushing: `cargo xtask precheck` (about a minute or less warm): `cargo fmt`, `velt fmt`,
  the file-size rule and clippy on the changed files only, the mechanical failures behind about
  half of all red pull request runs. `cargo xtask install-hooks` once per clone makes every
  `git push` run it (from any worktree; `git push --no-verify` skips it).
- Try a program: `cargo run -p veltc --bin velt -- run tests/golden/m1/hello.vlt`

## Rules for agents
0. Work in your own git worktree, never in the main checkout: `velt/main` stays on `main`, and each
   task gets `velt/branches/<task>` (`git worktree add ../branches/<task> -b <task> origin/main`;
   CONTRIBUTING.md "Working on several things at once"). If you find yourself in `main/`, create
   your worktree first. Use a build directory of your own for that worktree.
1. Stay within the crates your task owns. Treat the contracts as fixed (`ast.rs`, `hir`,
   `vir.rs` types, `velt_common`, public API signatures, `docs/internals/contracts/*`,
   `tests/golden/**`); if one must change, say so in your report and work around it meanwhile.
2. Code against the contracts, not against other crates' internals. Test your crate in isolation
   (hand-built inputs, unit tests, snapshot tests).
3. Before you finish: `cargo build --workspace` and `cargo test -p <your crates>` pass, and
   `cargo clippy -p <your crates> --all-targets -- -D warnings` is clean. Commit on your branch
   with a message like `frontend: lexer + Pratt parser for M1 subset`. Before opening the pull
   request, go through the self-check in CONTRIBUTING.md "Before opening a pull request" (Node
   comparison, docs, regression tests, no unwrap on user input, benchmarks, no sleeps, size).
   Push your branch and open a pull request (`Closes #N`), then post the self-check and the gate
   result as its first comment, not in the description; never push to `main`. The pull request
   title and description become the squash commit message: no "Generated with …" footers, session
   links or `Co-authored-by` trailers there or in commits, and commits keep the repository's
   configured author. CI runs the checks the pull
   request's changes need, and the whole gate in the merge queue (on Windows and macOS too when the change is
   OS-specific; CONTRIBUTING.md).
4. Compiler code must not panic on user input; report `Diagnostic`s. Internal invariant
   violations may panic with a message starting `ICE:`.
5. Every bug fix gets a regression test (end-to-end or unit). Every user-visible change updates
   the docs; `ts` code blocks in the docs must compile. It also files a docs-sync issue on
   `velt-lang/velt-lang.github.io` before merge, linked from the pull request, and a release
   comments its tag on those issues (CONTRIBUTING.md "The website's docs"); never open pull
   requests on the website from here.
6. Never commit secrets, tokens or personal data.

## Pitfalls (each cost a session real time)
- **Measuring on a busy machine.** With other builds running, wall-clock times are off by 2-4x.
  Compare base and head by instructions retired (`/usr/bin/time -l` on macOS, cachegrind on
  Linux), and run wall-clock A/Bs interleaved, best of N. For a std-only change, build both
  programs with the same compiler (std is read at compile time) and swap only the std files.
- **A new test must fail on a broken version.** Break the code on purpose (a sign, a constant,
  a check left out) and see the test fail before trusting it; a test that passes either way
  tests nothing.
- **Recursion stops inlining.** A hot helper that can call itself, directly or through its slow
  path, is not inlined by LLVM (a whitespace skip that called back after a comment cost 4-9%).
  Make the slow path return to the caller instead.
- **The string builder passed as a `string`** (std code calling `velt_rt_strbuf_*` externs): an
  argument after it that is a call makes the compiler copy the builder first, and the append
  goes to the copy. Compute such arguments into locals first, or use `StringBuilder`.
- **Module constants** initialized by a call are evaluated at every use; an array literal of
  numbers is static data.
- **`git stash` is shared by all worktrees.** Use a named stash and pop it by name, or copy the
  files aside, so you never pop another session's stash.
- **A shared `CARGO_TARGET_DIR` across worktrees** saves disk, but the `velt` binary finds
  `std/` next to the target directory, so it compiles against the other worktree's std (set
  `VELT_STD=$PWD/std`), and the tests that link a prebuilt runtime or std (playground, example
  apps, templates) fail with undefined symbols from the other branch. Use a build directory of
  your own (rule 0) when you need them to pass locally.
- **Pull request text:** the hygiene check rejects review and stacking notes ("stacked on #N")
  in the description, which becomes the commit message; put them in a comment.
- **Shell:** pass commit messages and PR bodies as files (`git commit -F`, `--body-file`):
  backticks inside `-m "…"` run as commands. In zsh, `for f in $list` does not split a string
  on spaces: use an array, or run the script with bash.
