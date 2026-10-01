# Contributing to Velt

Thanks for helping. This page covers building, testing, the coding standards, and how to
propose changes. How the compiler is put together is in [docs/internals](docs/internals/README.md).

## Reporting bugs

Open an issue with a minimal `.vlt` program, the command you ran, what happened and what you
expected, and `velt --version`. An internal compiler error (a message starting with
`internal compiler error`, exit code 101) is always a bug, whatever the input. Security problems
go through [SECURITY.md](SECURITY.md), not public issues.

## Building

You need Rust (stable) and a system linker: the Visual Studio Build Tools ("Desktop development
with C++") on Windows, `build-essential` on Linux, the Xcode command line tools on macOS. clang
16 or newer enables the LLVM backend and the release-mode tests.

```sh
cargo build --workspace
cargo run -p veltc --bin velt -- run tests/golden/m1/hello.vlt
```

Builds are large. Point `CARGO_TARGET_DIR` at a disk with room if needed; `VELT_GOLDEN_WORK`
moves the end-to-end tests' build directory the same way.

## Testing

| Command | What it runs |
|---|---|
| `cargo test -p <crate>` | one crate's unit and integration tests |
| `cargo test -p veltc --test golden` | the end-to-end tests: every `tests/golden/**/*.vlt` with an expected `.out`, built and run in debug and release mode (filter with `VELT_GOLDEN=m1/strings`) |
| `cargo test -p veltc --test docs` | the documentation tests: every `ts` code block under `README.md`, `docs/book`, `docs/reference`, `docs/std` and `docs/tooling` must compile (filter with `VELT_DOCS=<file:line>`) |
| `cargo test -p veltc --test standards` | the coding standards below (file sizes) |
| `cargo clippy -p <crate> --all-targets -- -D warnings` | lints |

Two tiers of the whole gate:

- **Fast**, while iterating: `pwsh scripts/check-all.ps1 -Fast` (Windows) or
  `scripts/check-all.sh --fast` (macOS, Linux). Build, clippy, all unit tests, the end-to-end
  tests in debug mode only, and `velt fmt --check` of `std` and `examples`.
- **Full**, once before you open a pull request: the same without `-Fast`/`--fast`, which adds
  the release-mode end-to-end tests. On Windows, `-Linux` also runs the gate in WSL;
  `scripts/linux-check.sh` runs it in Docker from macOS or Linux (`--services` starts PostgreSQL
  and Redis so the database tests run).

Every bug fix comes with a regression test: an end-to-end test (`tests/golden/**`) or a unit
test. A known bug without a fix can be added under `tests/golden/bugs/` (marked `.pending`: it is
reported but doesn't fail the run).

### Documentation tests

Code blocks fenced with ` ```ts ` are compiled by the documentation tests. The info string says
what to expect:

- `ts`: must compile. A snippet without `main` is completed: declarations stay at module level
  and other top-level statements move into a generated `async function main()`.
- `ts error`: must be rejected with a diagnostic (not an internal compiler error).
- `ts planned`: decided but not implemented; not compiled. The text must say **Planned**.
- `ts ignore`: an excerpt (a relative import, part of a program); not compiled.

If an example prints something, say what in a comment, and check it by running it
(`VELT_DOCS_DUMP=<dir>` writes every completed snippet to `<dir>`).

## Coding standards

Enforced by review, clippy and `cargo test -p veltc --test standards`:

- **File size**: source files aim for fewer than 400 lines; the hard limit is 800 lines (1000 for
  test files). Past that, split by concern into a module directory (`expr/mod.rs`, `expr/call.rs`,
  `expr/ops.rs`, …).
- **One concern per module.** Name modules after what they do (`lexer`, `resolve`, `drops`,
  `abi`), never `utils`, `misc` or `helpers`. `mod.rs` and `lib.rs` hold the public surface and
  the wiring, not the bulk of the logic.
- **Functions** stay short (aim for fewer than 60 lines); a big `match` delegates each arm to a
  named function.
- **Docs**: every module starts with a `//!` comment explaining its role; every `pub` item has a
  `///` doc. Comments explain *why*, not what.
- **Errors**: compiler stages return `Diagnostic`s and never panic on user input. No
  `unwrap()`/`expect()` on user-input paths; `expect("ICE: …")` only for true invariants. Tools
  and the CLI use `Result<_, String>` with actionable messages.
- **Tests** live next to the concern (`#[cfg(test)] mod tests`) for unit tests, in `tests/` for
  cross-module ones; shared test builders in `tests/common/`.
- **Naming**: Rust conventions; no abbreviations beyond the established ones (`ty`, `expr`,
  `stmt`, `def`, `hir`, `vir`).
- `.vlt` sources (std, examples, end-to-end tests except `errors/`) are formatted with
  `velt fmt`.
- `cargo fmt`, and clippy with `-D warnings`. No `#[allow(...)]` without a comment saying why.
- No dead code or commented-out code, and no speculative abstractions: build what is needed now,
  shaped so the next step slots in.
- **Dependencies**: few and mainstream; add them with `cargo add -p <crate> <dep>`.
- **Platforms**: Windows (MSVC), macOS and Linux, x86_64 and aarch64. No platform-specific code
  outside `velt_link` and `velt_rt` without `cfg`.

## Interfaces between stages

The AST (`velt_syntax/src/ast.rs`), HIR (`velt_sema/src/hir`), VIR (`velt_vir/src/vir.rs`),
`velt_common`, the runtime ABI and the documents in
[docs/internals/contracts](docs/internals/contracts/README.md) are contracts between crates.
Change them deliberately: describe the change and its reason in the pull request, and update the
contract document in the same change.

## Proposing a language change

Velt's rules (see [the Reference](docs/reference/README.md#the-velt-reference)): adopt
TypeScript's best parts and never JavaScript's bug sources; one way of doing things; add
something that is not TypeScript only where TypeScript can't express it at native speed.

1. **Open an issue** describing the problem, with real code that is awkward or impossible today.
2. **Write a design note** for anything beyond a small fix, in the style of
   [docs/internals/design](docs/internals/design): the syntax (as close to TypeScript as
   possible), the semantics, how it compiles, what it costs at run time, and the diagnostics
   users will see, including fixes for code that breaks.
3. **Show the performance impact** on `bench/` for anything that touches code generation or the
   runtime: the gate for semantic changes is every benchmark within 3% of the previous compiler.
4. Once the design is accepted, the implementation lands with end-to-end tests, documentation
   updates (the Reference marks unbuilt parts **Planned**), and a migration note if existing code
   breaks.

## Working on several things at once

Keep one checkout of `main` and a **git worktree per task**, side by side:

```
velt/
  main/                 git clone https://github.com/velt-lang/velt main   (stays on main)
  branches/
    json-unions/        one worktree per task or agent
    windows-wasm/
```

```sh
cd velt/main && git pull
git worktree add ../branches/<task> -b <task> origin/main   # start a task
# ... work, commit, push, open a pull request ...
git worktree remove ../branches/<task> && git branch -d <task>   # after it merged
```

Worktrees share one `.git`, so they are cheap and every branch is visible from every checkout.
Never edit `main/` itself; it only ever fast-forwards. Give each worktree its own build
directory (the default `target/` inside it, or `CARGO_TARGET_DIR` and `VELT_GOLDEN_WORK` on a
bigger disk), so parallel builds never wait on each other's locks. Agents follow the same layout:
one worktree per agent, created from `origin/main`.

## Commits and pull requests

Work on a branch and open a pull request against `main`; nothing is pushed to `main` directly.
Reference the issue it resolves (`Closes #123`) and say which gate you ran.

- **CI**: every pull request runs the fast gate on Linux. When a pull request is ready, add it to
  the **merge queue**: the queue runs the full gate on Linux, Windows and macOS against the pull
  request merged with the latest `main`, and merges it when all three pass. A nightly run adds
  PostgreSQL and Redis so the database tests run too.
- Pull requests are **squash-merged**: the pull request **title and description become the commit
  message** on `main` (`sema: infer throws through closures in recursive functions`). Keep the
  description to what changed, why, and how it was tested: no tool-generated footers, session
  links or co-author trailers, in the description or in your commits.
- Changes to the contracts (`ast.rs`, `hir`, `vir.rs`, `docs/internals/contracts/**`) or to
  language semantics need a maintainer's review (see `.github/CODEOWNERS`).

Never commit secrets, tokens or personal data.

By contributing, you agree that your contributions are licensed under the project's dual
MIT / Apache-2.0 license.
