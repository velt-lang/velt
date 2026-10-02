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
- End-to-end tests: `cargo test -p veltc --test golden` (filter: `VELT_GOLDEN=m1/strings`)
- Documentation tests: `cargo test -p veltc --test docs`
- Lint: `cargo clippy -p <crate> --all-targets -- -D warnings`
- Quality gate: `scripts/check.sh` on macOS and Linux, `pwsh scripts/check.ps1` on Windows. It
  runs the checks your changes need, selected from the files changed since `origin/main`
  (`cargo xtask affected` prints the plan and why). Run it while iterating and before you report.
  The whole gate is `scripts/check-all.sh` / `check-all.ps1`; the merge queue and `main` run it on
  three OSes, so you don't need to (changes to the build, toolchain, CI or scripts make `check.sh`
  select everything anyway). Install cargo-nextest so only the selected tests run, in parallel.
  Filter goldens with `VELT_GOLDEN=<substring>` while working on one area.
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
   with a message like `frontend: lexer + Pratt parser for M1 subset`. Push your branch and open
   a pull request (`Closes #N`, gate result); never push to `main`. The pull request title and
   description become the squash commit message: no "Generated with …" footers, session links or
   `Co-authored-by` trailers there or in commits, and commits keep the repository's configured
   author. CI runs the checks the pull
   request's changes need, and the whole gate in the merge queue (on Windows and macOS too when the change is
   OS-specific; CONTRIBUTING.md).
4. Compiler code must not panic on user input; report `Diagnostic`s. Internal invariant
   violations may panic with a message starting `ICE:`.
5. Every bug fix gets a regression test (end-to-end or unit). Every user-visible change updates
   the docs; `ts` code blocks in the docs must compile.
6. Never commit secrets, tokens or personal data.
