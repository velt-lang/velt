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
16 or newer enables the LLVM backend and the release-mode tests. Programs built in a checkout
link with the system linker; to try the bundled linker released toolchains use, see
[Linking](docs/internals/linking.md).

```sh
cargo build --workspace
cargo run -p veltc --bin velt -- run tests/golden/m1/hello.vlt
```

Builds are large. Point `CARGO_TARGET_DIR` at a disk with room if needed; `VELT_GOLDEN_WORK`
moves the end-to-end tests' build directories the same way (the goldens build in it, and the
LLVM, wasm, playground, panic, debugger and debug-info tests in subdirectories of it).
Debug builds of the compiler carry line tables only (file and line in backtraces and panics);
set `CARGO_PROFILE_DEV_DEBUG=full` when you want to inspect local variables in a debugger
(dependencies stay without debug info).

## Testing

| Command | What it runs |
|---|---|
| `cargo test -p <crate>` | one crate's unit and integration tests |
| `cargo test --workspace --test golden` | the end-to-end tests: every `tests/golden/**/*.vlt` with an expected `.out`, built and run in debug and release mode (filter with `VELT_GOLDEN=m1/strings`) |
| `cargo test -p veltc --test std_tests` | the standard library's behaviour tests: `velt test` over each `tests/std/<module>/`, in debug mode with a leak check and in release mode (filter with `VELT_STD_TESTS=http`) |
| `cargo test -p veltc --test docs` | the documentation tests: every `ts` code block under `README.md`, `docs/book`, `docs/reference`, `docs/std` and `docs/tooling` must compile (filter with `VELT_DOCS=<file:line>`) |
| `cargo test -p veltc --test example_apps` | `velt test --locked` for every example application with tests (`examples/apps/<app>/tests/`), each in a copy of the app (filter with `VELT_EXAMPLE_APPS=chat,seek`) |
| `cargo test -p veltc --test standards` | the coding standards below (file sizes) |
| `cargo clippy -p <crate> --all-targets -- -D warnings` | lints |
| `cargo xtask precheck` | the fast mechanical checks on the changed files only (below) |

Run the end-to-end tests with `--workspace`, not `-p veltc`: `-p` resolves the crates' features
differently from the gate's `--workspace` build (`velt_opt` loses `interp`), so switching between
the two rebuilds `velt_opt` and everything above it. (`--workspace --test golden` also runs
`velt_syntax`'s parser goldens, which take a second.)

The end-to-end tests take `VELT_GOLDEN=a,b` (files matching any of the substrings),
`VELT_GOLDEN_MODES=debug` or `release` (one build mode only), `VELT_GOLDEN_JOBS=<n>` (default
one per core, at most 16), `VELT_GOLDEN_TIMEOUT=<s>` (default 120), `VELT_GOLDEN_SHARD=<i>/<n>`
(the i-th of n interleaved shards) and `VELT_GOLDEN_STRICT=1` (failures under a `.pending`
directory fail the run too). The debug runs use the runtime's checking allocator
(`VELT_RT_DEBUG_ALLOC=1`; `=0` turns it off): a use after free (a read through a dangling
pointer, a write, a double free) or overflow aborts with `velt debug-alloc: …`, and a captured
variable's cell used by two tasks (a closure assigning it reached another task or HTTP request
without a copy) aborts with `velt debug-cells: …` (rt_abi.md "Counted objects").

velt_syntax's robustness test (`prefixes_never_panic_*`) cuts the goldens and parses what comes
before and after each cut, which must never panic. It cuts the kitchen sink and the goldens in
`VELT_ROBUSTNESS_FILES` at every character (comma-separated paths such as
`tests/golden/lang/foo.vlt`; `cargo xtask check` sets it to the goldens you changed), and the
other goldens where each token starts or ends plus a fixed sample of characters.
`VELT_ROBUSTNESS=full` cuts every golden at every character (about eight times the work): the
nightly gate sets it, and so does `cargo xtask check` (and the merge queue, and the label `ci:full`)
when velt_syntax, a crate it depends on, `rust-toolchain.toml` or the root `Cargo.toml` changed,
or when the changes can't be told.

The quality gate is `cargo xtask check` (crates/xtask), with two ways to run it:

- **The checks your changes need**, while iterating and before you push: `scripts/check.sh` (macOS,
  Linux) or `pwsh scripts/check.ps1` (Windows). The files changed since the merge base with
  `origin/main`, plus uncommitted and untracked ones, select the checks: a crate's tests and those
  of the crates depending on it, the end-to-end tests when a change can affect compiled programs
  (or only the goldens you touched), the documentation tests for docs, clippy for Rust changes,
  `cargo fmt`, clippy and the unit tests of `tests/difftest` (a crate outside the workspace) when
  it changes, and `npm ci` plus `npm test` (tsc and the unit tests) of the VS Code extension when
  `editors/vscode/` changes (skipped without npm, except in CI). A Rust file where only comments
  changed runs the tests and doctests of its crate (and of the crates compiling its sources),
  clippy, `veltc`'s unit tests and the file-size test (`standards`), nothing else. The WebAssembly
  goldens (`wasm_goldens`, about half of the test time) run only for changes to WebAssembly code
  (a path containing `wasm`, or LLVM's target setup) and for changed goldens; the nightly run
  checks them all on Linux, Windows and macOS. Changes to the build, the toolchain, CI or the
  scripts select everything. `cargo xtask affected` prints the plan and why; the rules are in
  `crates/xtask/src/plan.rs`. Goldens run in debug mode (`--golden-modes release` for the other);
  `--part` runs some parts (`lint`, `test`, `golden`, comma-separated; `wasm` runs only the
  WebAssembly goldens, which `test` includes).
- **Everything**: `scripts/check-all.sh` or `pwsh scripts/check-all.ps1` (`--fast` / `-Fast`:
  goldens in debug mode only). The merge queue runs it on Linux (and on Windows and macOS for
  OS-specific changes), and the push to `main` on Windows and macOS when the queue checked Linux
  only, so you need it locally only when you want that certainty before queueing. The queue
  skips the WebAssembly goldens when the change doesn't touch WebAssembly code, and otherwise
  runs them in parallel shards next to the goldens' (`--part wasm`, `VELT_WASM_SHARD=<i>/<n>`),
  not in the lint-and-test job (`VELT_WASM_GOLDENS=skip`). On Windows, `-Linux` also builds and tests the committed `HEAD` in
  WSL (`scripts/test-linux.sh`, in a clone at `~/velt-linux`; run the script with `--dir
  /mnt/d/...` to keep it on the bigger disk, see below);
  `scripts/linux-check.sh` runs the gate in Docker from macOS or Linux (`--services` starts
  PostgreSQL and Redis so the database tests run).

Install [cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`): with it the
gate runs only the selected tests, and test binaries in parallel; without it every test runs, one
binary at a time (over ten minutes; the gate prints a warning). Local runs keep incremental
compilation on (CI turns it off).

**Before pushing**, `cargo xtask precheck` runs the checks behind most avoidable CI failures, on the
files changed since the merge base with `origin/main` (plus uncommitted and untracked ones) only:
`cargo fmt --check` of the changed crates, the gate's `velt fmt --check std examples` when
anything under `std/` or `examples/` changed or the formatter (`velt_fmt`) or a crate it depends on
did, the file-size rule (below) on the changed Rust files, and clippy with
`-D warnings` on the changed crates and the crates depending on them (`tests/difftest` too when
it changed). It runs every check and then lists what failed with how to fix it; warm, it takes
well under a minute. `cargo xtask install-hooks` installs a git `pre-push` hook that runs it
before every push; worktrees share the clone's hooks, so once per clone is enough. The hook never
replaces a `pre-push` hook it didn't install (`--force` does, keeping the old one as
`pre-push.orig`), and `git push --no-verify` skips it. The hook checks the **working tree**, not
the commits being pushed: uncommitted and untracked files count, so commit or stash your work in
progress first, or a scratch file can block an unrelated push and an uncommitted fix can hide a
bad commit. (With a relative `core.hooksPath`, git looks for hooks in each worktree, so install
them in each.)

### Golden or `velt test`?

- **A golden** (`tests/golden/**`) when the program's output is the specification: the language
  (`m1`–`m4`, `lang`), compile errors (`.err`), exit codes and panic messages (`.code`,
  `.stderr`), how `console.log` prints a value (std/fetch_inspect), and runtime or ownership
  cases where the exact order of what is printed (across tasks, callbacks, drops) is what's being
  tested.
- **`velt test`** (`tests/std/<module>/*.test.vlt`) for the standard library's behaviour: a
  `describe` per subject, an `it` per case, `expect` checks instead of printed text, so a
  failure names the case and its line. They run with `cargo test -p veltc --test std_tests`
  (`VELT_STD_TESTS=http` for one module), in debug mode and in release mode. The debug run checks
  that each test file's process frees every block it allocates (`VELT_RC_STATS=1`, the goldens'
  `// check: no leaks`) and uses the checking allocator as the goldens' debug runs do, so close
  every server and connection a test opens. Matchers for one
  module's types go in `std/test/<module>.vlt` (`velt/test/http`'s `toHaveStatus`).

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

### The standard library

The layout and its reasons: [docs/internals/design/std-layout.md](docs/internals/design/std-layout.md).
`scripts/std-lint` checks the rules marked (lint); `cargo xtask precheck` runs it on the std files
you changed, and the gate on all of std. Its allow list (`scripts/std-lint/allow.vlt`) only
shrinks.

- **Modules are folders** (lint). A folder with an `index.vlt` is the module `velt/<folder>`;
  every other file belongs to the nearest module above it and is internal. No files directly in
  `std/`, and no `x.vlt` next to an `x/` folder. `prelude/` (implicit) and `lang/` (what
  implements the language and its globals) are never imported.
- **`index.vlt` is the API** (lint): named re-exports only (`export { Deque } from
  "velt/collections/deque";`), no `export *`, no code; every name it exports has a doc comment.
  Every public name has one import path (lint).
- **One exported class per file** (lint), named after it (`Deque` in `deque.vlt`), with its small
  companions; free functions grouped by the operation family they implement; native bindings
  (`declare function velt_rt_*`, `declare system function`) in one file per module; tables in
  files of their own. At most 400 lines (lint), except files generated by a script, which say
  so in their header.
- **Headers** (lint): the first line is `// velt/<module>: <role>` in an `index.vlt` and
  `// velt/<module> (internal): <role>` elsewhere (`// Prelude: …`, `// Global …` in the
  prelude); `velt run scripts/std-lint/main.vlt --fix-headers` writes the module part.
- **Imports**: std code imports from the file that defines a name (`velt/io/error`), user code
  and tests from the module (`velt/io`).
- **A new module**: its folder and `index.vlt`, a row in the areas of std-layout.md §3, its page
  under `docs/std/`, and end-to-end tests under `tests/golden/std/` or `tests/std/`.
- Callbacks rethrow what they throw (`map<U, E>(f: (x: T) => U throws E): U[] throws E`);
  element-returning methods clone, iteration and callbacks borrow; performance notes are in
  [std/README.md](std/README.md).

## Interfaces between stages

The AST (`velt_syntax/src/ast.rs`), HIR (`velt_sema/src/hir`), VIR (`velt_vir/src/vir.rs`),
`velt_common`, the runtime ABI and the documents in
[docs/internals/contracts](docs/internals/contracts/README.md) are contracts between crates.
Change them deliberately: describe the change and its reason in the pull request, and update the
contract document in the same change.

## Proposing a language change

Velt's rules (see [the Reference](docs/reference/README.md#the-velt-reference)): adopt
TypeScript's best parts and never JavaScript's bug sources; one way of doing things; valid
TypeScript runs as it does in Node, and Velt adds its own opt-in features wherever they make
programs faster. Velt is a native backend language, not a way to compile to JavaScript, so a
proposal for a Velt-only feature is welcome when it shows the speed it buys.

1. **Open an issue** describing the problem, with real code that is awkward or impossible today.
2. **Write a design note** for anything beyond a small fix, in the style of
   [docs/internals/design](docs/internals/design): the syntax (as close to TypeScript as
   possible), the semantics, how it compiles, what it costs at run time, and the diagnostics
   users will see, including fixes for code that breaks.
3. **Show the performance impact** on `bench/` for anything that touches code generation or the
   runtime: the gate for semantic changes is every benchmark within 3% of the previous compiler.
   `bench/nightly.sh` counts instructions with cachegrind; for a runtime change,
   `bench/runtime_ab.sh` links the same programs against `origin/main`'s runtime and yours; for
   a compiler or std change, `bench/compiler_ab.sh BASE HEAD` builds them with each commit's
   toolchain.
4. Once the design is accepted, the implementation lands with end-to-end tests, documentation
   updates (the Reference marks unbuilt parts **Planned**), and a migration note if existing code
   breaks.

Maintainers: how pull requests are reviewed, queued and planned is in
[docs/internals/maintaining.md](docs/internals/maintaining.md).

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
cargo xtask install-hooks                                   # once per clone: precheck before each push
# ... work, commit, push, open a pull request ...
git worktree remove ../branches/<task> && git branch -d <task>   # after it merged
```

Worktrees share one `.git`, so they are cheap and every branch is visible from every checkout,
and the `pre-push` hook from `cargo xtask install-hooks` covers all of them: each push runs
`cargo xtask precheck` in the worktree it comes from.
Never edit `main/` itself; it only ever fast-forwards. Give each worktree its own build
directory (the default `target/` inside it, or `CARGO_TARGET_DIR` and `VELT_GOLDEN_WORK` on a
bigger disk), so parallel builds never wait on each other's locks. The `velt` cargo builds keeps its
build cache in that build directory (`velt-build-cache`, at most 500 MB), not in `~/.velt`. Agents follow the same layout:
one worktree per agent, created from `origin/main`.

A new worktree starts with an empty build directory, and building the dependencies takes minutes.
[sccache](https://github.com/mozilla/sccache) shares them between worktrees: `cargo install
sccache --locked`, then `export RUSTC_WRAPPER=sccache` (or `[build] rustc-wrapper = "sccache"` in
your `~/.cargo/config.toml`). It caches the dependencies, which compile the same everywhere; the
workspace crates build incrementally as before.

Building in WSL on Windows: keep the build directory and temporary files on the Windows disk you
build on (`/mnt/d/...`, with `TMPDIR` there too), not in the WSL home directory. WSL's own
filesystem is a disk image on the system drive that grows with every build and never shrinks on
its own.

## Commits and pull requests

Work on a branch and open a pull request against `main`; nothing is pushed to `main` directly.
Reference the issue it resolves (`Closes #123`), and go through the self-check below before
opening it.

- **CI**: every pull request runs the checks its changes need (as `scripts/check.sh` selects
  them) on Linux, as three parallel jobs: lint, test and golden; a change to OS-specific code
  (below) is also linted (fmt, clippy) on Windows and macOS, so `cfg(windows)`-only findings
  show before the queue. The label `ci:full` makes them check everything (add it before
  pushing, or close and reopen the pull request). When a pull
  request is approved, the maintainer who approves it adds it to the **merge queue** in the same
  step (`gh pr merge <n> --auto`): the queue runs the whole gate against the pull
  request merged with the latest `main`, and merges it when it passes. It checks Linux, plus
  Windows and macOS when the change touches OS-specific code (the runtime, code generation and
  linking, `veltc`, `vpm`, the standard library, goldens; comment-only edits don't count;
  `cargo xtask affected` says which).
  When the queue checked Linux only, the push to `main` then runs the whole gate on Windows and
  macOS and opens an issue when it fails. A nightly run adds PostgreSQL and Redis so the
  database tests run too.
- Pull requests are **squash-merged**: the pull request **title and description become the commit
  message** on `main` (`sema: infer throws through closures in recursive functions`). Keep the
  description to what changed, why, and how it was tested: no tool-generated footers, session
  links or co-author trailers, in the description or in your commits.
- The required check **`hygiene`** (`cargo xtask pr-lint`, `.github/workflows/hygiene.yml`)
  enforces that on the title, the description and every commit. It fails on tool footers
  (`Generated with …`) and trailers (`Co-authored-by:`), links to chat or agent sessions,
  absolute machine paths (`/Users/<name>/`, `/home/<name>/`, `C:\Users\<name>\`,
  `/private/tmp/…`), review-process wording ("review round", "addressed the review", "Stacked on
  #N": say these in a comment), and commits not by the pull request's author: GitHub must
  attribute each commit to the author's account (any email verified there; the GitHub noreply
  address `<id>+<login>@users.noreply.github.com` keeps it private), or its email must be in the
  repository variable `PR_LINT_AUTHORS`. Merge commits, such as "Update branch", are not checked
  for authorship. Text in code spans and blocks is exempt from the wording rules, but not from
  the path rule or from whole session URLs.
  Editing the description re-runs it. Before opening a pull request, check the description
  locally: `cargo xtask pr-lint --title "<title>" --body-file <file>` (commits since
  `origin/main` and `git config user.email` by default).
- **Dependent changes** go in a stack of pull requests (the
  [gh-stack](https://github.com/github/gh-stack) extension: `gh extension install
  github/gh-stack`). Start one with `gh stack init`, `gh stack add` and `gh stack submit`, or join
  existing pull requests with `gh stack link <bottom> ... <top>`; `gh stack sync` rebases the
  layers. Each layer gets its own CI and review, and when the bottom one merges the next is
  retargeted to `main`. Changes that don't depend on each other stay separate pull requests, so
  the merge queue can test them in parallel.
- **Size**: keep a pull request under about **1,500 changed lines** (added plus deleted, tests
  included); split larger work into a stack. Small pull requests get reviewed sooner and need
  fewer change rounds. `hygiene` notes a larger one in the checks summary without failing.
- **Reviews** end with a line `Verdict: APPROVE` or `Verdict: CHANGES NEEDED`. After `CHANGES
  NEEDED`, push fixes for every required item and say in a comment which commit addresses which
  item; the re-review looks at the new commits and those items, and an approval queues the pull
  request at once.
- Changes to the contracts (`ast.rs`, `hir`, `vir.rs`, `docs/internals/contracts/**`) or to
  language semantics need a maintainer's review (see `.github/CODEOWNERS`).

### Before opening a pull request

Most required changes in reviews are things the author could have checked. Before opening a pull
request, run `cargo xtask precheck` (or let the pre-push hook run it) and `scripts/check.sh`, and
go through this list:

1. **Node**: every new or changed golden that is valid TypeScript was run with Node
   (`node --experimental-strip-types <file>.ts`, a copy of the `.vlt` file) and prints the same
   output; where Velt deliberately differs, the golden or the docs say why.
2. **Docs**: user-visible changes are in the docs (the reference, `docs/`, the README), with
   compiling `ts` examples, and have a docs-sync issue on the website repo, linked from the pull
   request ([The website's docs](#the-websites-docs)); otherwise there is no user-visible
   change.
3. **Tests**: each bug fix has a regression test that fails without the fix.
4. **Panics**: no `unwrap()` or `expect()` on a path user input reaches (source, `velt.json`,
   command-line arguments, files, the network); `expect("ICE: …")` only for invariants.
5. **Performance**: a change on a hot path (the runtime, generated code, the compiler's inner
   loops) comes with benchmark numbers before and after (`bench/`), within ±3%.
6. **Timing**: tests and goldens order events by awaits, channels or observed state, not by
   sleeps or wall-clock limits.
7. **Size**: under about 1,500 changed lines, or split into a stack.
8. **std**: a changed std API has its `index.vlt` re-export with a doc comment, its docs page
   and a test; `std-lint` passes (`cargo xtask precheck` runs it).

Post the result as the first comment on the pull request: each item with what you did (or "n/a"),
and the gate you ran. Keep it out of the description: the description becomes the commit message
and says what the change does and how it was tested, not the review process. A missing self-check
comment is a required change in review.

## The website's docs

The docs on [veltlang.com](https://veltlang.com) live in their own repository,
[`velt-lang/velt-lang.github.io`](https://github.com/velt-lang/velt-lang.github.io), and describe
the latest release. Its docs agent writes them from an issue queue there; this repository feeds
the queue and does not open pull requests on the website. `docs/` here stays the compiler team's
reference and the website's first draft, so a user-visible change still updates it in the same
pull request.

A change is user-visible when it adds or changes public API or behavior: syntax or semantics, a
standard-library module or function, a diagnostic, a `velt` command or flag, `velt.json`, or what
a program prints or how fast it runs where the docs make a claim. Feed the queue at two moments:

- **Before merging such a pull request, file a docs-sync issue** on the website repository,
  titled `velt: <what changed>`, and link it from the pull request. A user-visible pull request
  isn't ready for the merge queue until its issue exists:

  ```sh
  gh issue create --repo velt-lang/velt-lang.github.io --label docs-sync \
    --title "velt: <what changed>" \
    --body "Source: velt-lang/velt#<pr>. <What users see, with a short example, and the pages it affects.> Not yet released."
  ```

  (The website's "Docs sync" issue form asks for the same: the source pull request, what users
  see, the pages affected.)
- **When you release**, comment the tag on every open docs-sync issue whose change the release
  ships (step 3 of [Releases](#releases)):

  ```sh
  gh issue comment <n> --repo velt-lang/velt-lang.github.io --body "Released in velt vX.Y.Z."
  ```

  An issue without a release comment means merged but not released: the docs agent does not
  document it yet. The comment is its signal that the change is live.

## Releases

A release is a tag `v<version>` on `main`, where `<version>` is the workspace version in
`Cargo.toml`:

1. Open a pull request that sets `version` under `[workspace.package]` in `Cargo.toml` (and
   updates `Cargo.lock` with `cargo build`), and merge it.
2. Tag the merged commit and push the tag: `git tag v0.2.0 origin/main && git push origin v0.2.0`.
3. Once the release is published, comment `Released in velt v0.2.0.` on every open docs-sync
   issue on [`velt-lang/velt-lang.github.io`](#the-websites-docs) whose change it ships
   (`gh issue list --repo velt-lang/velt-lang.github.io --label docs-sync --state open`, then
   check each source pull request against the tag with `git merge-base --is-ancestor`).

The tag starts the `release` workflow (`.github/workflows/release.yml`). It builds the toolchain
for Linux x86_64 and arm64 (in Debian 11, so it runs on glibc 2.31 and newer), macOS arm64 and
x86_64 and Windows x64 with `scripts/package.*`, bundling the lld that `lld.yml` builds (from
source, kept in the Actions cache; a cold build takes up to an hour), installs every archive with
`scripts/get-velt.*` and smoke-tests it through the launcher, also on machines without a C
toolchain (`scripts/smoke-clean.*` in Debian slim and Windows Server Core containers), publishes
a GitHub release with the archives, `SHA256SUMS`, its signature `SHA256SUMS.sig` and the two
installers, rewrites the signed index of releases (`releases.json` on the `index` release,
`.github/workflows/release-index.yml`), and finally installs the published release on every
platform the way users do. A tag that does not match the `Cargo.toml` version fails before anything is
built. Versions with a suffix (`0.2.0-rc.1`) become pre-releases; they are not "latest", so the
`releases/latest/download/...` install commands keep pointing at the last full release. To test
the pipeline without publishing, run the workflow by hand (Actions → release → Run workflow): it
builds and smoke-tests the archives and keeps them as workflow artifacts, and checks that the
signing key matches the key velt is built with.

**Signing.** `scripts/release-sign.sh` signs `SHA256SUMS` and `releases.json` with the release
key, the secret `VELT_RELEASE_SIGNING_KEY` (an Ed25519 private key, PEM) of the GitHub
environment `release` (Settings → Environments), which allows only `main` and tags `v*` to use
it, so a workflow run from another branch, with changed scripts, can't read the key. The jobs that
sign (`signing-key` and `publish` in `release.yml`, `index` in `release-index.yml`) run in that
environment. The script checks each signature against the public keys velt is built with
(`velt_toolchain::signature::RELEASE_PUBLIC_KEYS`, also embedded in `scripts/get-velt.sh`; a
test keeps them in line): the **signing key**, and a **backup key** whose private half is kept
offline, never in GitHub. The launcher, `velt target add` and `get-velt.sh` accept a signature
by either and refuse anything else. Keep an offline copy of both private keys: without one of
them, no release can be signed in a way installed launchers trust.

**Replacing the signing key** (it leaked or was lost):

1. Make a new backup key pair offline (`openssl genpkey -algorithm ed25519`), and get its public
   key in hex: `openssl pkey -in <file> -pubout -outform DER | tail -c 32 | xxd -p -c 64`.
2. In a pull request, make the old backup key the first entry of `RELEASE_PUBLIC_KEYS`, add the
   new backup key as the second, and drop the old signing key. Update the two lines of
   `release_keys` in `scripts/get-velt.sh` (the base64 that `scripts/release-sign.sh
   --public-key` prints; a test checks they match), and the backup's proof signature
   (`BACKUP_KEY_PROOF` in `velt_toolchain::signature`), made with the new backup key:
   `printf 'velt backup release key <new backup hex>' | openssl pkeyutl -sign -rawin -inkey
   <new backup>.pem -in /dev/stdin | xxd -p -c 128`.
3. **Re-sign the earlier releases.** The new launcher no longer trusts the old signing key, so
   it would refuse every earlier `SHA256SUMS.sig` (and `velt toolchain install 0.2.x`, and
   packages pinned to them). For each earlier release, check its `SHA256SUMS` against a record
   the old key never controlled (a copy kept when it was published, or the hashes of the
   archives you can rebuild), then sign it with the old backup and replace the signature:
   `VELT_RELEASE_SIGNING_KEY="$(cat old-backup.pem)" scripts/release-sign.sh --backup SHA256SUMS`
   and `gh release upload v<x> SHA256SUMS.sig --clobber`.
4. Put the old backup's private key into the `release` environment's `VELT_RELEASE_SIGNING_KEY`
   (from then on it is the signing key) and release. Launchers that carry the old backup key
   accept the release; the launcher it brings trusts only the new pair.
5. **If the old signing key leaked, revoke it:** add its public key (hex) to
   `.github/release-revoked-keys.json` in the same pull request. The index of releases that
   release writes, signed by the old backup, names it in `revoked_keys`; every launcher that
   reads that index refuses the old signing key from then on, for good (`<root>/revoked-keys`),
   because the backup comes after it in their trust list. (A key never revokes one after it: a
   leaked signing key can't revoke the backup.)

**If the backup key leaks** (the signing key is fine): release a launcher that drops it, signed
with the signing key as usual, with a new backup key in its place (steps 1 and 2 without
dropping the signing key, and without step 3: earlier releases are signed with the signing key).

**What this can't do.**

- Revoking reaches a launcher only when it reads the index of releases (installing a version
  or `velt toolchain list --available`), and only launchers from the release after 0.2.1 on
  know `revoked_keys`. Until then, and in every `get-velt.sh` from the releases that embed the
  leaked key, what it signs is accepted. The holder of the leaked key can serve those launchers
  an older, unrevoking index, but not one older than the newest they have seen.
- The backup key can't be revoked through the index (only a key after it could): release a
  launcher without it instead (above).
- The backup only helps launchers that carry it: those from the first release after 0.2.0
  on. 0.2.0's launcher and installer know only the signing key.
- The freshness check of the index of releases is one series for the official releases
  (`<root>/index-seen/official`) whichever key signs, so it continues across a rotation. A
  launcher applies an index's revocations before that check, and refuses (and never remembers)
  an index written more than a day ahead of its clock, so the leaked key can't block the
  revoking index by serving one dated far ahead.
- `get-velt.sh` doesn't read stored revocations: it trusts every key it embeds, on a fresh
  install and when it installs into a root that has `revoked-keys` (it leaves that file there,
  so the launcher it installs still refuses those keys). A fresh install trusts a leaked key
  until its launcher reads an index that revokes it.
- Tampering with `<root>/revoked-keys` or `<root>/index-seen/` on the user's machine is outside
  what this protects against: whoever can write the root can replace `bin/velt` itself.
- Only the official keys revoke. Another build's keys (`$VELT_INSTALL_PUBLIC_KEY`) can't
  withdraw them. A launcher that sees an index revoke a key after its signer ignores it and
  warns, because no release does that: it means a misconfigured release or a leaked signing key.

**Yanking.** To withdraw a release with a serious bug, add it to `.github/release-yanks.json`
(`{"0.1.2": "miscompiles closures; use 0.1.3"}`) in a pull request, merge it, and run the
`release-index` workflow (Actions → release-index → Run workflow). Launchers then skip the
version when they choose the newest a requirement accepts (an exact `=0.1.2` still installs
it), and `velt toolchain list --available` shows why. Remove the entry and run the workflow
again to unyank it. The release itself stays.

**Advisories.** To tell users of a release about a problem without withdrawing it, give the
version an object in `.github/release-yanks.json`: `{"0.2.0": {"advisory": "TLS certificates
are not checked", "security": true}}` (an object may also say `"yanked"`), then run the
`release-index` workflow. A launcher running that version says so on stderr once a day, from the
last index it read (no network), and names the newest release of the same minor without a
problem. `scripts/release-index.sh` checks the file (a string, or an object with only `yanked`
and `advisory` strings and a `security` boolean) and fails the workflow on anything else, so a
typo never reaches the launchers.

Never commit secrets, tokens or personal data.

By contributing, you agree that your contributions are licensed under the project's dual
MIT / Apache-2.0 license.
