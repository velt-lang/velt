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
- All quality gates: `pwsh scripts/check-all.ps1` on Windows, `scripts/check-all.sh` on macOS and
  Linux. Run the fast tier (`-Fast` / `--fast`: end-to-end tests in debug mode only) while
  iterating and the full gate once before you report. Filter with `VELT_GOLDEN=<substring>`
  while working on one area.
- Try a program: `cargo run -p veltc --bin velt -- run tests/golden/m1/hello.vlt`

## Rules for agents
1. Stay within the crates your task owns. Treat the contracts as fixed (`ast.rs`, `hir`,
   `vir.rs` types, `velt_common`, public API signatures, `docs/internals/contracts/*`,
   `tests/golden/**`); if one must change, say so in your report and work around it meanwhile.
2. Code against the contracts, not against other crates' internals. Test your crate in isolation
   (hand-built inputs, unit tests, snapshot tests).
3. Before you finish: `cargo build --workspace` and `cargo test -p <your crates>` pass, and
   `cargo clippy -p <your crates> --all-targets -- -D warnings` is clean. Commit on your branch
   with a message like `frontend: lexer + Pratt parser for M1 subset`. Push your branch and open
   a pull request (`Closes #N`, gate result); never push to `main`. CI runs the fast gate on the
   pull request and the full three-OS gate in the merge queue (CONTRIBUTING.md).
4. Compiler code must not panic on user input; report `Diagnostic`s. Internal invariant
   violations may panic with a message starting `ICE:`.
5. Every bug fix gets a regression test (end-to-end or unit). Every user-visible change updates
   the docs; `ts` code blocks in the docs must compile.
6. Never commit secrets, tokens or personal data.
