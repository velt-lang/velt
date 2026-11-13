# `velt` CLI — CONTRACT

```
velt build [<file.velt>] [-o <out>] [--release] [-g] [--backend cranelift|llvm] [--target <triple> | --host <package>] [--cpu <name>] [--emit vir|llvm|obj|exe] [--locked] [--no-generators] [--json] [-v] [--timings]
velt run   [<file.velt>] [--release] [-g] [--target <triple>] [--cpu <name>] [--backend cranelift|llvm] [--locked] [--no-generators] [-- <program args>...]
velt check [<file.velt>] [--target <triple>] [--perf [--deps]] [--fix[=safe|review]] [--json] [--locked] [--no-generators] [-v]
velt check --ts-compat [<file|dir>...] [--fix[=safe|review]] [--json] [--locked] [-v]
velt dev   [<file.velt>] [--exe [--release] [-g] [--cpu <name>] [--backend <name>]] [--locked] [-v] [--timings] [-- <program args>...]
velt test  [<file|dir>] [--filter <text> [--exact]] [--release] [--locked] [--jobs <n>] [--watch]
velt test  [<file|dir>] [--filter <text> [--exact]] [--release] [--locked] --no-run [--json]
velt new   <name> [--template app|cli|api|websocket|lib] [--lib]
velt init  [--template <t>] [--name <name>] [--force]
velt init  --editor vscode [--dir <d>] # only .vscode/launch.json + extensions.json, kept if present
velt clean [--cache]
velt cache warm [--background]
velt completions bash|zsh|fish|powershell
velt help  [<command>]                 # = velt <command> --help
velt add   <pkg>[@<req>] [--path <dir>]
velt install [--locked]
velt update
velt publish [--native-artifacts <dir>] [--native-only]
velt native build [--target <triple>]
velt fmt [<file|dir>...] [--check]
velt lsp [--stdio]                     # language server (VS Code extension: editors/vscode)
velt playground [--port <n>] [--host <addr>]   # browser playground (default 127.0.0.1:8090)
velt doc [<file|dir>...] [--std] [--format html|json|types] [-o <dir|file|->]     # API docs (HTML, or JSON for the docs website), std's TypeScript declarations
velt registry serve [--dir <d>] [--port <n>] [--host <addr>]   # package registry server (default 127.0.0.1:8091)
velt doctor                            # checks toolchain setup + smoke test
velt toolchain list [--available] | install <v> [--default] | remove <t> [--force] | self-update | prune [--dry-run] | default [<t>] | which | link <name> <prefix> | unlink <name> | --version   # the launcher's (#948)
velt --version                         # velt <ver> (<git hash> <host triple>)
```
- **Single file**: `build <file>` writes `./target/velt/<stem>` (`.exe` on Windows) relative to the
  current dir, object file next to it. If the file lives inside a package, its deps are installed first.
- **Package mode** (no file argument): finds `package.velt` upward from the cwd, builds its entry, output
  `<pkg>/target/velt/<name>[.exe]`. Library-only packages can't be run.
- `build --json` (additive): instead of diagnostics on stderr, one JSON document on stdout:
  `{"executable": string|null, "debugInfo": bool|null, "lldbScript": string|null, "diagnostics",
  "errors", "warnings"}`. `executable` is the absolute path of the linked program (`null` on
  failure or with `--emit obj|vir|llvm`); `debugInfo` is whether the program has line
  information for debuggers: debug info was asked for and the backend emits it for the target
  (`false` for Cranelift builds for Windows linked by `link.exe` instead of the bundled lld:
  they carry no DWARF; `null` if the options could not be resolved);
  `lldbScript` is the absolute path of the toolchain's `share/velt/lldb/velt_lldb.py` (`null`
  if missing). The diagnostics are those of `check --json`, with a non-source failure (linker,
  missing runtime) as an error with `location: null`. Exit codes as for `build`. The VS Code
  extension's `velt` debug type relies on it.
- **Member selection** (additive, #1855): `check`, `test`, `build`, `fmt` and `doc` take
  `--workspace`, `-p <glob>` / `--package <glob>` (repeatable; `-p=`/`--package=` too),
  `--affected [<base>]` (`--affected=<base>`) and `--fail-fast`; `run` and `dev` take `-p
  <member>` (`run`: only before its file, whose following arguments are the program's). A glob
  matches a member's name, or a scoped name's part after the `/`. They are read before the
  command's own options
  (`commands::members::intercept`): outside a workspace they are an error, and so is a file
  argument with them. Without them, those five commands at a workspace root outside its packages
  select every member (`run`/`dev` there: an error listing the runnable members). One selected
  member: the command runs in its directory in this process (for `run`/`dev`, always exactly
  one), a relative `-o` resolved against the starting directory. Several: `--watch`, `-o` and
  `--json` are errors, and the workspace is installed once (`--locked` passed on; not for `fmt`),
  then `velt <command> <the other arguments>` runs in each member's directory, at most half the
  available cores at once, a member after the selected members it depends on, output captured and
  printed per member under `Member <name> (<dir>)` / `Failed …`, then `Finished `velt <cmd>` on
  <n> members: <p> passed[, <f> failed (<names>)][, <s> skipped after a failure (<names>)]`; exit
  code: the first failed member's, else 0. `build` skips members with no entry (libraries).
  `--affected` takes `git diff --name-only --relative <base>` plus untracked files below the
  workspace root, each owned by its innermost member except under its `target/` (the workspace
  file or lockfile: every member), plus the members that depend on them; without a base, `git
  merge-base HEAD @{upstream}`, and an error when `CI` is set.
- `test --filter <text>` (additive): only the tests whose name contains `<text>` run (`--exact`:
  only the test named `<text>`; a file with none is skipped; no match at all prints `no tests
  match` and exits 0). `test --no-run` (additive): builds each test file's harness without running
  it, into `target/velt/test/debug/` (with variables for debuggers, as `velt build`), and prints
  the executables' paths; with `--json`, one document on stdout: `{"harnesses": [{"file",
  "executable", "tests", "env", "debugInfo"}], "lldbScript", "diagnostics", "errors", "warnings"}`
  (`file` the test file, `executable` the harness, both absolute; `tests` the test functions it
  runs, in order; `env` the variables to run it with, `VELT_SCRIPT` for `process.argv[1]`;
  `debugInfo` and `lldbScript` as for `build --json`). The diagnostics are those of every harness
  build and of test files that do not parse, as in `check --json`. Exits 1 when a harness did not
  build. `--json` needs `--no-run`, `--exact` needs `--filter`, and `--watch` does not go with
  `--no-run`. The VS Code extension's "Run test" and "Debug test" rely on it.
  `test --no-cache` (additive, #1855): runs every test file. A `velt test` in a package that
  passed with every file kept writes `target/velt/test/member-stamp` (the build stamp's format;
  key over the command line after member selection, the `velt` binary and the `VELT_*`
  environment; `objects` = tests passed; entries = its harnesses' build-stamp entries from before
  the run and the directories discovery listed), and removes it otherwise; `velt test` on
  members (several, or one with `-p`) doesn't start a member whose member stamp for the same
  command is unchanged, printing `Unchanged <name> (<dir>): <n> passed`. `--watch` has no member
  stamp. Without it, a file whose harness is
  unchanged since all its tests passed, and whose tests reach nothing outside the program (the
  build stamp's `outside` record, per file of a harness), is not run: the runner prints its kept
  report after `running <path> (cached)`; a file whose tests do reach outside prints `running
  <path> (ran: not cacheable, it uses <what>)`. Kept in `<harness>.test-results.json`.
  `test --jobs <n>` / `-j <n>` (additive): how many test files run at once (default: one per
  core); the output is the same for any `n`, in file order.
  Time limits (additive): a test file's process that is still running `$VELT_TEST_TIMEOUT` ms
  (default 600 s) after it started is killed; the test it was running fails as `FAILED <test>
  (timed out after <n> s with this test running)` and a new process runs the tests after it.
  With `VELT_RC_STATS=1` the process prints the marker line `<mark>done` after its last test and
  the `afterAll` hooks (`<mark>` holds a token made for each file's run, `$VELT_TEST_MARK`);
  one still running `$VELT_TEST_LEAK_WAIT` ms (default 120 s) after that line is killed and the
  file fails as `still running <n> s after its tests: a server, a connection or a timer left
  open`. The time the tests take does not count toward that wait.
- `new` / `init` (additive): every template also writes `.vscode/launch.json` (one
  `{"type": "velt", "request": "launch", "name": "Debug"}` configuration) and
  `.vscode/extensions.json`, both kept if present. `init --editor vscode` writes only those, in
  `--dir` (an existing directory) or else at the root of the package around the cwd (or in the
  cwd outside one), never overwriting, and takes no other option.
- Backends: default `llvm` for `--release` when clang is found, else `cranelift` (with a one-line
  stderr note). `--emit llvm` prints LLVM IR (no clang needed). The search
  (`velt_codegen_llvm::clang_search`; `velt doctor` says which clang and why): `$VELT_CLANG` first
  and final. On macOS, every `clang` and `clang-NN` in an absolute PATH entry (an empty or relative
  one means the current directory, whose files this search must not run), Homebrew's `llvm` and
  `/usr/local/bin/clang`, ranked: any clang from LLVM above Apple's, then by version, the earlier on
  a tie; `/usr/bin/clang` last, not started when one from LLVM will do. Elsewhere the first usable
  in a fixed order: `clang` on PATH, the standard install dirs, `clang-NN` from new to old. A
  candidate counts if `--version` answers within 30 s with LLVM 16 or newer (Apple clang 15); the
  one about to be chosen must also compile a small IR function for the host (not asked of
  `$VELT_CLANG`, which may be a cross compiler). One that fails is passed over for the next; if none
  compiles, the first is kept and the build reports it. Nothing falls back during a build: a clang
  that fails there on the small IR too is reported by name, as not a compiler bug, and asked again
  by the next build. A candidate that doesn't answer in time costs the build that wait and one line
  on stderr. The next build asks it again, since a first start can be slow (macOS checks a new
  executable); after a second timeout within the hour, builds leave it alone for an hour unless no
  other will do. `velt doctor` asks it regardless, and an answer ends that.
  Release builds run `velt_opt` (Speed) before either backend.
- Codegen units (additive): the LLVM backend splits a large program into units compiled by
  parallel clang processes, at most one per core (`velt_codegen_llvm::emit_objects_timed`); the
  first unit's object is `<exe>.o` (`.obj`), the others `<exe>.cgu<N>.o` beside it, and all are
  linked; objects of higher-numbered units from an earlier build are removed. With
  `$VELT_CODEGEN_UNITS` unset, the count depends on the program's size only (not on the
  machine): one unit below 32 000 VIR statements, else one per 16 000, at most 4.
  `$VELT_CODEGEN_UNITS` = N overrides it (N units, at most the core count; `1`: no split).
  `--emit obj` always writes one object and removes none.
- **CPU** (additive): `--cpu <name>` (`velt build`, `run`, `test`, `dev --exe`), else
  `$VELT_CPU`, else the root package's `build.cpu`, reaches both backends as
  `CodegenOptions::cpu` (`commands::cpu::resolve_cpu`). Names: `baseline` (alias `x86-64`),
  `x86-64-v2|v3|v4`, `native`, or a CPU name of the target's architecture (clang's names,
  `velt_codegen_cl/src/cpu_names.rs`), checked by `velt_codegen_cl::check_cpu` (a CPU of
  another architecture, or `native` for one other than the host's, is an error from `--cpu`
  and does not apply from the others; a name no architecture has is an error from all three;
  WebAssembly takes none). `--cpu` without `--exe` is an error for `velt dev`. Unset, x86_64
  code is for `x86-64-v2` (`velt_codegen_cl::DEFAULT_X86_CPU`), other targets for their
  default; `velt run`/`velt test`/`velt dev --exe` without `--release` use `native`. LLVM gets
  `-march=<name>` (x86_64) or `-mcpu=<name>`; Cranelift the level's ISA flags, its preset of
  that name, or the baseline. x86_64 code above the baseline calls `velt_rt_check_cpu` first
  in `velt_main` (rt_abi.md "CPU check"; added to the VIR by the driver, `--emit llvm`
  included) with the features that backend's code uses: for a CPU name, Cranelift's preset or
  what clang defines for `-march=<name>` (`velt_codegen_llvm::required_features`). `velt dev`'s JIT
  always uses the host's features.
- **WebAssembly** (additive): `--target wasm32-wasip1` (alias `wasm32-wasi`)
  and `--target wasm32-unknown-unknown` build `./target/velt/<stem>.wasm` (object `<stem>.o`) with
  the LLVM backend (always; `--backend cranelift` is an error) using LLVM's `opt`/`llc`
  (`$VELT_LLVM_BIN`, else rustup's `llvm-tools`, `PATH` or Homebrew's LLVM, else, unless
  `$VELT_LLVM_BIN` is set, the clang of the pinned zig, downloaded on first use: #2587),
  `wasm-ld` (`$VELT_LINKER`, the bundled lld, rustup's `rust-lld`, `wasm-ld` on PATH) and
  `libvelt_rt_wasm.a` (`$VELT_RT_LIB`, else cargo's `target/<triple>/<profile>/`, else
  `<prefix>/lib/<triple>/`, where a release ships the `wasm32-unknown-unknown` one). WASI builds also need wasi-libc (`$VELT_WASI_SYSROOT`, else
  rustup's `wasm32-wasip1` target). `run` executes WASI modules with `$VELT_WASM_RUNNER <module>
  <args>` or `wasmtime run --dir=. <module> <args>`; browser builds also write the JS glue
  `velt_web.mjs` next to the module, and `run` executes them with `node velt_web.mjs <module>`.
  No TCP, HTTP server or child processes on WebAssembly (link error with a note); `fetch` links
  but rejects with `IoError` `ENOTSUP` (no network), so code that only builds `Response`s runs.
- **`--host <package>`** (additive, #1736; `velt build` only): a build for the JavaScript
  platform of host package `<package>`, one of the build's packages whose `package.velt` has
  `host` (manifest.md, [workers.md](workers.md)). The compiler names no platform: the package
  gives the target (`--target` with another triple: "`--host <p>` builds for `<t>`: leave out
  `--target <t2>`"), the condition added to the target's, the entry and the files to scaffold.
  Errors: "no package `<p>` among the build's dependencies (add it with `velt add <p>`)",
  "package `<p>` is not a host package", "`--host <p>` builds a package" (no package), a target
  the JavaScript glue doesn't run. Output: a directory (`-o <dir>`, else
  `target/velt/<name>-<condition>/`) that must be new, empty, hold only the host's scaffolded
  files, or be an earlier host build's (its `.velt-host`); else "`<dir>` holds files a `--host`
  build did not write". Warns without `--release`. A host module the program calls that neither
  the glue (`velt:…`, declared only by std) nor exactly one package declaring it provides is an
  error. `velt run --host` is an error ("cannot run a `--host` build"). `-p <member> --host <p>`
  takes `<p>` as the option's value.
- `playground` (additive): serves a page where programs are edited, compiled
  on the server to `wasm32-unknown-unknown` (`POST /api/compile[?release=1]`, body = source →
  `200 application/wasm` or `422` diagnostics text with paths as `main.velt`) and run in the
  browser (Web Worker + `velt_web.mjs`). Only `std/…` imports are accepted. Needs the browser
  runtime (`cargo build -p velt_rt_wasm --target wasm32-unknown-unknown`).
- `doc` (additive): HTML API docs from exported items, their public members,
  their signatures as written and the doc comment right above each declaration: a JSDoc
  `/** … */` comment or a block of `///` lines, with JSDoc tags (`@param`, `@returns`,
  `@throws`, `@example`, `@deprecated`, `@see`, `{@link}`); plain `//` is not a doc comment
  (`docs/internals/design/doc-comments.md`). A comment block at the top of a file, followed by a
  blank line, documents the module. No paths: the package's `src/`
  (its `.velt`, `.ts` and `.tsx` files, not `.d.ts`; `src/lib.velt` is named after the package) into `<pkg>/target/doc`; paths: those files and
  directories into `./target/doc`; `--std`: the standard library. `-o` overrides the output
  directory. Writes `index.html`, one page per module, and a client-side search index.
  `--format json` (additive): the same documentation as one JSON document, `api.json` in the
  output directory, or the `.json` file `-o` names, or stdout for `-o -`. It is
  `{ schemaVersion, title, modules }`; `schemaVersion` (1) goes up when a field changes meaning
  or goes away, and fields may be added without it. The fields are in https://veltlang.com/docs/tooling/cli/
  "JSON output"; the docs website renders its API pages, search index and std catalog from it.
  `--format types` (additive, with `--std` only): the standard library's TypeScript
  declarations as the npm package `velt` (`package.json` with an `exports` map, an
  `index.d.ts` per public module, `globals.d.ts`) into `-o` (default `target/velt-types`);
  https://veltlang.com/docs/tooling/cli/ "TypeScript declarations".
- `registry serve` (additive): serves a registry directory (default: `$VELT_REGISTRY` when it
  is a directory, else `$VELT_HOME/registry`) over HTTP (protocol in manifest.md "Remote
  registries"). `publish` uploads to the package's registry: by default the public registry
  https://registry.veltpm.com, and copies into a directory only when `$VELT_REGISTRY` names one
  and the package has no `registry` (manifest.md "The default registry").
- `registry user add|remove|token <name> [--dir <d>] [--open]` (additive): the users of a registry
  directory (`<dir>/.auth/users.json`, token hashes only). `add` and `token` print the new token
  on stdout, once. A registry with users accepts writes only with a user's token
  (`Authorization: Bearer <token>`, see `login`); without the users file it is open. `remove` of
  the last user deletes the file (opening the registry) only with `--open`. `remove` also drops
  the user from every package's owners, and reports those packages, warning about any left
  without an owner.
- `registry owner add|remove <pkg> <user> [--dir <d>]` (additive): an administrator's change of
  a package's owners, made in the registry directory without a token (may remove the last owner).
- `registry serve` exits 1 without serving when `$VELT_REGISTRY_TOKEN` is set and the registry is
  open (the variable used to protect a server).
- Commands that install (`add`, `install`, `update`, `build`, `run`, `check`, `test`, `dev`) print
  `warning: `<name>` <version> is yanked (pinned by velt.lock.json)` for each yanked locked version
  whose index they read (a locked version already in the cache reads none: manifest.md "The
  default registry", #2840).
- `yank <pkg>@<version> [--undo]` (additive): sets or clears the version's `yanked` flag in the
  package's registry (local, or remote: owners only). Resolution skips yanked versions unless
  `velt.lock.json` pins them; `add` without a version picks the newest stable version not yanked
  (the newest pre-release not yanked if there is no stable one).
- `owner list|add|remove <pkg|@scope> [<user>]` (additive): a package's owners on a registry
  server, or with `@scope` (no `/name`) a scope's (`/api/v1/scopes/<scope>/owners`, registry-core
  servers); `list` prints one per line on stdout; an error for a local registry.
- `add @scope/name[@<req>]` (additive): the version's `@` is the one after the scope's.
- `login [<registry-url>] [--device | --token-stdin]` / `logout [<registry-url>]` (additive; the
  two flags together are an error): without a URL, the registry `publish` would use in the
  current directory (by default https://registry.veltpm.com; an error when that is a local
  directory). `login` first refuses a plain `http://` URL that is not loopback
  (rule below), before it reads or opens anything. Then:
  - with `--token-stdin`, or without a flag when stdin is not a terminal: it reads one line from
    stdin (as before; nothing is asked of the registry);
  - otherwise it asks the registry for browser sign-in (`GET /api/v1/login`, `manifest.md`
    "Remote registries"). Any answer but a 200 with a `browser` page, and a registry that can't be
    reached (a warning), fall back to reading the token from stdin, prompting on stderr without
    echo (`--device` warns first);
  - with browser sign-in and no flag, the loopback flow: it prints the page's URL (with a hint
    about `--device`) on stderr, opens it (`open` on macOS, `xdg-open` on other Unix,
    `cmd /d /v:off /c start "" "<url>"` on Windows, where a URL with `%`, `!` or `"` is only
    printed), and waits up to 5 minutes for the browser's callback, then fails suggesting
    `--device`. A browser that can't be opened is not an error;
  - with `--device`: the device flow (an error when discovery says `"device": false`); it prints
    `Open <verification_uri> and enter the code <user_code>` on stderr, tries to open
    `verification_uri_complete`, and polls until signed in, declined, or `expires_in` (at most an
    hour) has passed.

  A sign-in prints `Logged in to <url> as <user> (token valid until <expires>)` on stderr; the
  token is never printed, and the terminal shows only fixed messages for the registry's `error`
  words. The token, pasted or signed in, is stored as the token of that registry in
  `$VELT_HOME/credentials.json`: `{"registries": {"<url>": {"token": "…"}}}`, keys with the scheme
  and host lowercased and no trailing `/`, written atomically (temporary file, synced, renamed),
  mode 0600 on Unix, an owner-only protected DACL on Windows; the terminal doesn't echo the
  token. `logout` first revokes the stored token on the registry (`DELETE /api/v1/token` with it:
  2xx revoked, 401 already invalid, 404/405 an older server that keeps it, which is a warning, as
  is a failed request), then removes the entry either way. Writes to a registry server (`publish`, `yank`, `owner
  add|remove`) send `$VELT_REGISTRY_TOKEN` if set, else the token stored for exactly that
  registry, else none. A token is never sent (nor stored) for a plain `http://` URL whose host is
  not loopback (a host part with `@`, `?`, `#` or `\` is never loopback); that is an error.
- `search <text> [--json]` (additive): `name  version  description` lines on stdout (columns
  aligned; the description cut to the terminal's width with `…`, not cut when stdout is not a
  terminal) for the packages of the package's registry (or `$VELT_REGISTRY`; by default
  https://registry.veltpm.com) that match the text
  by name, keywords or description (ranking in manifest.md "Search"), each with the version `add`
  picks: the newest stable version not yanked (the newest pre-release not yanked if it has no
  stable one). `--json` prints the registry's answer (`{"packages": [...]}`) instead.
- `check` (additive): parse + sema, no lowering, codegen or link. Input resolution: with a file,
  that file and its imports (the file's package, if any, supplies dependencies); the root module
  need not define `main` (a library module; every function body is still checked), a `main` that
  is there is validated as for `build`, which, like `run`, still requires one. Without a file, in
  a package: every source module (`.velt`, `.ts`, `.tsx`; not `.d.ts`) under `src/` and `tests/`
  (recursively, skipping `target/`, `node_modules/`, hidden and symlinked directories and nested
  packages, i.e. directories with their own manifest, whose `package.velt` is never a module; no
  other directory; `vpm::sources::walks_into`), loaded together in one front-end run with the package's root module: the entry
  (`package.entry`, default `src/main.velt`), which must define a valid `main`, or, when there is
  no configured entry and no `src/main.velt`, `src/lib.velt` as a library module. Every other
  module is a library module; one whose module path is taken or reserved (`src/std/x.velt`, a
  `src/util.velt` next to a dependency `util`) gets a name no import can write instead of an
  error (`build` does not load it). Files in one directory whose names differ only in the
  source extension (`src/dup.velt`, `src/dup.ts`) are an error per group, located in the first
  file and naming all of them relative to the package root. A configured entry that is missing is an error naming it (as
  for `build`). Package dependencies are installed. Every diagnostic the front end reports
  appears once, even for a module several roots import: all files' syntax errors, then the type
  errors of the modules without syntax errors (changed: formerly none when any file had a
  syntax error). The modules with syntax errors report only those; a name imported from one
  (directly or through re-exports) that it doesn't declare but whose source has that word
  reports nothing, nor do its uses, as the parser may have dropped its declaration. `build` and `run` still stop at syntax errors. Exit 0 without errors (warnings allowed), 1 with errors, 101 on an
  internal error.
  Diagnostics go to stderr as for `build`; `--json` prints instead one JSON document on stdout:
  `{"diagnostics": [{"severity": "error"|"warning"|"note", "message", "location", "labels":
  [{"location", "message"}], "notes": [string]}], "errors": n, "warnings": n}` where a
  `location` is `{"file", "line", "column", "endLine", "endColumn"}` (1-based; columns count
  bytes, as in the text output) or `null`. A non-source failure (unreadable input, broken
  package) is an error diagnostic with `location: null`. `-v` prints stage timings to stderr.
  Additive: every JSON diagnostic also has `"code"` (the lint rule, `null` for the check's own
  diagnostics; changed: a ts-compat finding's is `ts-compat/<rule>`, formerly `<rule>`),
  `"fixes"` (each `{"title", "applicability": "safe"|"review", "edits": [{"location",
  "replacement"}]}`; empty for none) and `"fix"` (the first fix when it is one edit, as
  `{"location", "replacement", "title"}`, else `null`).
- `check --ts-compat <file|dir>...` (additive): checks exactly the given files (a directory: its
  source modules, found as for a package, recursively; a directory without any is an error; an
  explicit `.d.ts` path is an error) in one front-end run, the first as the root and the rest as
  extra roots, all library modules, inside their package (if any). All files must share one
  package root (the nearest directory above each with a manifest), or all have none; otherwise
  it fails (exit 1) with an error naming two of the files and their packages ("lint one package
  per run: …"). Each group of them with the same module path is an error, as in a whole-package
  check. Then it lints, with `velt_tscompat`, those of them that have no error diagnostic
  located in them (none when loading or parsing failed). Findings follow the check's
  diagnostics (in text, after a blank line), ordered by file and position. Text: each is a
  diagnostic (its severity, message and notes) whose last note is `ts-compat(<code>)`. JSON: the
  same diagnostic with `"code": "<code>"` and `"fix": {"location", "replacement", "title"}` or
  `null`. A relative import (`./`, `../`) of a file that is not among the given files is the
  finding `outside-import`. Exit 1 on any error (from the check or an error-severity finding), 0
  otherwise.
- `check --ts-compat` without paths (additive; formerly a usage error): the paths are the
  `tsCompat` folders (`manifest.md`) of the package around the current directory, named relative
  to it, and their source modules are checked and linted as above (an empty folder adds none).
  It fails (exit 1, a `location: null` diagnostic with `--json`) outside a package ("… there is
  no `package.velt` …"), when the manifest has no `tsCompat` ("package `<name>` has no `tsCompat`
  folders to lint …"), when a listed folder is not a directory ("package `<name>`: `tsCompat`
  folder `<dir>` does not exist" / "is not a folder"), or when the folders hold no source
  module. Plain `velt check` never runs the ts-compat rules, `tsCompat` or not.
- Lint rules (additive; docs/internals/design/lint-profiles.md). Each source file has a profile:
  portable (in a `tsCompat` folder), native (the package's other files, or every user file
  outside a package) or dependency (other packages', std's: never reported or fixed). The
  `perf/…` rules (`velt_lint::PERF_RULES`) have the level `lint` in `package.velt` sets
  ([manifest.md](manifest.md)), `hint` by default. Plain `velt check` runs the rules on the
  syntax tree and types only when one of them is at `warn` or `deny`; `--perf` runs them at any
  level but `off`, and the rules on the optimized program too, which build the program up to
  optimized VIR (a release build with line information, no code generation; a program that does
  not build gives a note without location instead). A hint is a `note`, `warn` a warning,
  `deny` an error (exit 1). In a portable file only fixes that stay TypeScript are kept, and a
  finding left without one is a note. Findings follow the check's diagnostics, ordered by file
  and position; each is a diagnostic whose notes end with a note naming its first fix
  and the flag that applies it (`fix: <title> (…--fix)`, or `--fix=review`), then
  `<family>(<rule>)`. `--deps` (only with
  `--perf`) adds the optimizer's findings in dependencies as notes without fixes. A
  `// velt-allow(<code>[, <code>…]): <reason>` comment suppresses those codes on the next line
  with code (alone on its line) or on its own line (after code); `velt-allow-file(…)` in the
  whole file; `<family>/*` covers a family. A warning reports an allow without a reason, of an
  unknown code, or that suppressed nothing although its rules ran (for `<family>/*`: one of
  the family's rules); a `lint.rules` code that is no rule is a warning without location. The ts-compat findings honour the same comments and
  levels (`lint.rules` may set `ts-compat/<rule>`).
- `--fix` (`=safe`, the default) / `--fix=review` (additive): check and lint, apply the first fix
  of each diagnostic that the mode allows (Safe; Review too) whose edits overlap no fix applied
  before, then check again. If the files no longer check (more errors than before), every fix
  is undone and they are applied one at a time, keeping each after which they still check. It
  prints `velt: fixed <n> problem(s) in <m> file(s)` (or `velt: nothing to fix`) and one `velt:
  not applied, the program did not check after it: <title> (<message>)` per undone fix to
  stderr, then the check and lint of the result as without `--fix`. JSON: the report of the
  result with `"fixed": {"fixes", "files", "undone": [string]}`. Not with `--std`.
- Linking (additive): debug builds (no `--release`) link the runtime as a
  shared library (`libvelt_rt_shared.so` / `.dylib`, `velt_rt_shared.dll` + `.dll.lib`) found
  next to `velt`, its parent directory or `<prefix>/lib`, with an rpath to it (Windows: the DLL is
  copied next to the executable), plus a generated `<stem>.entry.<o|obj>` holding `main`.
  Release builds, `$VELT_RT_LIB` and `$VELT_RT_LINK=static` link the static runtime; so do debug
  builds when no shared runtime is installed. The linker (additive, #803): the bundled lld
  (`<prefix>/lib/velt/lld[.exe]`, in a checkout the Rust toolchain's `rust-lld`) with the
  target's link kit (`<prefix>/lib/targets/<triple>/`, `kit.stamp` of the current format) when
  both exist, else the system linker (`link.exe` / `cc`); `$VELT_LINKER=bundled` makes a missing
  bundled linker an error, `$VELT_LINKER=system` uses the system linker, any other value is a
  linker program run with the system linker's arguments. Cross-OS targets link only with the
  bundled linker. A target other than the host's (#856) links statically against the runtime of
  its target pack, `lib/targets/<triple>/` (never the host's runtimes, whose file names it
  shares); without the pack the build fails naming `velt target add <triple>`. `velt run
  --target` accepts WebAssembly and targets of the host's OS only.
- `velt target list|add|remove` (additive, #856): `list` prints `<triple>  (this machine)`, then
  each installed pack (`(installed)` / `(broken: …)`; `.`-prefixed directories are an `add` in
  progress and not listed), then the other release targets (`(not installed: velt target add
  <triple>)`). `add <triple>...` (release targets only; the host's prints that it is built in)
  downloads `velt-<version>-target-<triple>.tar.gz` from
  `$VELT_INSTALL_BASE_URL/releases/download/v<version>/` (https, redirects followed) and checks
  it against `<prefix>/lib/targets/PACKS.sha256` (written by the release workflow), or, when the
  toolchain lists no hash for it, against the release's `SHA256SUMS` (fetched first), checked
  against `SHA256SUMS.sig` with the release keys (`velt_toolchain::signature`: a signature by
  the signing key or the backup key passes; `$VELT_INSTALL_PUBLIC_KEY`, one or more hex keys
  separated by commas, replaces them); an unsigned release or a signature
  that does not match fails. It unpacks only regular files under `<triple>/` and installs the
  pack into `<prefix>/lib/targets/<triple>/` once its kit and runtime are complete and its stamp
  names this velt (`velt_link::kit::toolchain_id`), swapping a previous pack out whole. `--from
  <file>` (one triple) installs a local pack checked the same way, or against a `SHA256SUMS`
  beside it; `--unverified` (with `--from` only) installs one that cannot be checked. `remove`
  takes a release target's name only and deletes its pack; the host's target cannot be removed.
  Exit 1 with a message on any failure. A kit whose stamp names another velt is not used: the
  build falls back (host) or fails naming `velt target add` (other targets). Linux static links with the system linker use
  `-fuse-ld=mold`/`lld` when `mold`/`ld.lld` is on `PATH` (falling back to the default linker if
  that link fails). A link whose inputs (objects, runtime library, settings, the linker:
  `velt_link::linker_identity`) are unchanged since the executable was last linked is skipped
  (`<exe>.link-stamp` beside it). The linker writes `<stem>.link-<pid>-<n>` beside the
  executable, which is then renamed into place (#2026): a program started from its path while it
  is linked again (two `velt test` runs in one package) starts the old executable or the new one,
  never a file the linker is still writing. On Windows, an executable that cannot be replaced
  while it runs is renamed to `<exe>.old-<pid>-<n>` first and removed once nothing runs it.
- Build stamp and build cache (additive, #1612). `build`, `run` and `test` with `--emit exe`
  (the default) and no `--report` skip parsing, checking and codegen when `<exe>.build-stamp`
  matches: the compiler (version, executable size and time), the options, a generated root's
  source, the `VELT_*` environment (but `VELT_BUILD_CACHE*`, `VELT_BUILD_STAMP`,
  `VELT_CACHE_STATS`, `VELT_TEST_PLAN`, `VELT_JOBS`), clang for LLVM builds, every file the build read (the
  session's source map and the configuration files module resolution read) by content hash (the
  size and time it had when it was read first, not recorded when it was modified less than 2 s
  before it was read, and not trusted when not older than the stamp), what
  module resolution looked at (`vpm::probe`: every path probed is still a file, a directory or
  missing as it was, every directory listed has the same entries), the manifests and lockfile
  of the package graph, and by size and time the unit objects and the executable it wrote (so
  an output another build or tool rewrote is a change), extra link inputs, and the std cache's
  objects the link read (`std` lines; stamp format `velt-build-stamp 6`, whose paths name their
  directory from a table after the header, `d <dir>` and `~<n>/<name>`; a path holding a line
break makes a stamp that never matches). The objects it names
  must exist; the link step then runs on the unit objects and the `std` objects (and checks its
  own stamp). The stamp ends with `end <entries>`
  and is renamed into place whole. Only builds without diagnostics write a stamp; a build that
  does not match removes it first, as does a build of the executable that writes none
  (`VELT_BUILD_STAMP=off`, `--report`). `VELT_BUILD_STAMP=off|0` disables it.
  Codegen goes through the build cache (`velt_build_cache`): `$VELT_BUILD_CACHE` (a directory, or
  `off|0|false|no|none`), default `$VELT_HOME/build-cache` (a `velt` in a cargo target directory, one holding cargo's `.rustc_info.json` or whose
  profile directory holds its `.cargo-lock`: `<target dir>/velt-build-cache`, limit 500M from
  `.cargo/config.toml`); the root gets a `CACHEDIR.TAG` on the first write when it is new, empty or
  holds only what the cache makes (a directory with other files is used, never adopted), and
  `velt clean --cache` and garbage collection act only on a root that has it, removing only layout directories,
  `warm/` and the marker (the root too when it is then empty); layout `v5/<kind>/` (keys are
  BLAKE3 hashes; `v4` used SHA-256)
  with kinds `cl` (Cranelift compiled functions, Cranelift's incremental-cache key with the
  Cranelift version), `stdfn` (the std cache below: objects of single std functions, file
  entries), `llvm`
  (unit objects, keyed by clang's path, size, time, reported version and arguments, the host triple
  and the IR),
  `std-ast` (std files' parses, `velt_syntax::encode_module`, keyed by the compiler identity,
  `AST_FORMAT`, plain TypeScript or not and the source's length, CRC-32 and 64-bit hash; only
  parses without diagnostics of files read from disk, never an editor's unsaved buffer; every
  command that loads a program uses them, `check` and the language server included), and
  `clang-probe` (`clang --version`'s output, keyed by the candidate's resolved path, size and
  modification time, so a release build doesn't start clang to learn its version: only for a native
  program whose real file is named `clang…`; a script, or a link to something else, is a wrapper and
  is asked each build; and whether it compiles for the host, keyed also by its version; and that a candidate timed out
  once or is left alone, for `--version` and for the target check apart, keyed by the same three,
  its name and the hour, the value being the time it was written).
  Entries under 16 KiB are packed: 64 shards per kind by the key's first byte, each `<nn>.pack`
  (values) + `<nn>.idx` (56-byte records: key, offset, length, 8-byte checksum (CRC-32) of the
  value, minutes since 1970 of the last use; the last record of a key wins) appended under an
  exclusive lock of `<nn>.lock`; a value whose checksum does not match is a miss, and the next
  store appends it again. Larger ones are files, `<kind>/large/<2 hex>/<62 hex>`: the value's
  length (8 bytes) and checksum (8), then the value, synced and renamed into place, used at their
  modification time; one that fails its check is removed (a miss) and stored again. File entries
  (`Cache::put_file`/`path_of`/`touch`, for files a linker reads) are plain files in
  `<kind>/files/<2 hex>/<62 hex>` with the length and checksum in a `.check` sidecar, checked
  before the path is handed out; stored, and removed when damaged, under `<kind>/files.lock`.
  Their time of use is the sidecar's: the file keeps its size and time while it is cached. Limit
  `$VELT_BUILD_CACHE_SIZE` (`parse_size`: `500M`, `2G`, bytes; default 2 GiB) on the disk space
  the files take (allocated blocks), enforced after a build that stored entries, at most hourly
  (`gc-stamp`): entries used least recently go until 3/4 of it, packs with a quarter unreferenced
  are rewritten, and other layout directories (`v1`) are removed. The directory must be writable only by trusted
  users: entries are used as they are. A hit refreshes its time of use
  when that is over an hour old. `-v` appends
  `(<n> of <m> functions|codegen units from the build cache)` to the codegen line, and a `stamp`
  stage, with `(unchanged: nothing to compile)` when it matched.
- Std cache (additive, #1728 step 3; for all it serves, off by default). Release executables (`--emit exe`) built by
  LLVM without debug info (`-g`), with `VELT_STD_OBJECT=1`,
  take std functions out of the program: each std function that is not small (40 statements),
  not a `$dup` rename, reaches only statics with relocations or written (named by it or by a
  small function it may inline, and what their relocations reach) that can be named (no other
  static of the program has the same contents: such statics become `Export` statics named
  `_Vs<content fingerprint>`, vir.rs invariant 13, and the functions their relocations name
  are exported) and makes no indirect call is keyed by BLAKE3 over `velt std function
  1`, `build_cache::compiler_identity()` (version line, executable size and time), clang's path,
  size, time and reported version, clang's optimization flag (`VELT_LLVM_OPT`), the target, the CPU
  and the
  optimization level; then, for it and each small internal function it reaches through small
  ones (in that order), its symbol, `velt_vir::fingerprint::Canon::text` (`Mode::Link`) and its
  parameter attributes; then, for each function that code calls without compiling it (what its
  object declares), its symbol, params, return type and parameter attributes. A found entry is
  looked up again when linking; gone by then, the build falls back as below. Before optimization,
  each found key (kind `stdfn`, `Cache::path_of`), and each missing one when the missing
  functions come to at least 1,000 statements (else they stay in the program), makes the function
  an `Import` declaration; functions their objects may reference are made `Export`. After
  optimization the objects of the functions still declared, closed over what those objects call,
  are linked; a missing one is compiled then, alone (itself, the small functions it may inline
  with their bodies, declarations of what they call, the statics they name; optimized at
  `Speed`; LLVM, one unit; the entry declared, vir.rs invariant 5) and stored (`Cache::put_file`).
  If one fails, the build optimizes the program as lowered, without the cache. Each object is
  placed as soon as it is looked up again or stored, before codegen (builds of one output wait for
  each other on `<object stem>.std.lock`, saying `velt: waiting for another build of this output`
  on stderr; not `--host` builds, which use no objects), in `<object stem>.std/<n>.<o|obj>`
  beside the program's objects (hard links to the cache's files, copies where the file system has
  none, replaced by each build; MSVC's linker reads an input without an extension as
  `<name>.obj`); one gone from the cache by then makes the build fall back as above. The link
  reads those, and the stamp's `std` lines name them.
  The miss threshold is 1,000 statements unless `VELT_STD_OBJECT_MIN_MISSES` says otherwise. `VELT_STD_OBJECT_STATS=1` prints, on stderr, the std
  statements by reason (`served`, `small`, `$dup`, `statics with identity`, `callbacks`), each
  linked symbol (`velt: std cache: links <symbol> (<n> statements)`) and `velt: std cache: <n>
  found, <m> compiled, <k> no longer called`.
- Std cache in debug builds (additive, #2460; `VELT_STD_OBJECT=0` turns it off). Debug executables
  (`--emit exe`, not `--report`, not WebAssembly, with either backend and with `-g`, for a CPU with
  everything the target's default CPU requires) look up every std function that is not one
  `velt_opt::debug_inlined` names, not a `$dup` rename, and reaches only statics that can be named
  (and, where Cranelift keeps thread-local values on thread keys, none of those),
  when the cache has held any `stdfn` entry. The key is as above with `velt std function digests
  1`, no clang (`llvm`), `cpu` unset, and BLAKE3 digests of each function's symbol, code and
  attributes in place of the texts. A found entry makes the function an `Import` declaration and
  its object is placed as above, before optimization; one that can't be placed, or isn't found,
  stays in the program. A debug build never starts clang or stores an entry, except with
  `VELT_STD_OBJECT_FILL` set (`velt cache warm`) and clang found: misses are then compiled alone
  (at `Speed`, for the default CPU) and stored, and one that fails stays in the program.
  `VELT_STD_OBJECT_STATS=1` ends with `velt: std cache: <n> found, <m> compiled, <k> left in the
  program`. Marked `@inline` counts as small in the release path too: such a function stays in
  the program and goes into its callers' objects; marked `@noInline` is never small, nor
  compiled into another function's object. The JIT host and generator hosts never use the
  cache. The stamp's `outside` line is worked out before functions become declarations.
- Std cache in release builds by default (additive, #2405; `VELT_STD_OBJECT=0` turns it off,
  `=1` is the path above). Release executables as above, without `VELT_STD_OBJECT`, look up the
  std functions marked `@noInline` (`vir::Inline::Never`) that the path above would serve (it
  serves a small `@noInline` function too, never compiled into another function's object), and
  that are not an ordinary function checking a CPU extension (`velt_simd_has_<ext>`) itself or
  through what it references, nor gated with such a function among those its object declares,
  with its keys (the program's CPU, clang) but over every internal function reached through
  internal functions not marked `@noInline` in place of the small ones, preceded by a part that
  says so (`own copies of every callee up to @noInline`), when the cache has held
  any `stdfn` entry and the program has such a function; the key's last flag says release (a
  debug build's says debug, so neither serves the other). Its object has internal copies of its
  own of those functions, except those whose address it or a static it reaches takes, which are
  declarations keeping their bodies, and the program exports only those and what the object
  declares. Each function has two keys: with the program's parameter attributes, and without
  `noalias`, `readonly` and `frozen` (whole-program facts) for an object compiled with none of
  them; the first found is linked. A found entry is placed before optimization; the function
  keeps its body through velt_opt, which inlines into it but runs no per-function pass on it after
  the first (`velt_opt::optimize_served`), and becomes an `Import` declaration afterwards
  (`Served::take_out`), and what only it called is removed (`velt_opt::remove_unused_functions`),
  so every other function is optimized as without the cache; a miss stays in the program, and no
  clang starts for it, except with `VELT_STD_OBJECT_FILL` set and clang found (misses compiled
  alone for the program's CPU, at `Speed`, and stored under both keys where they differ). After
  optimization the objects of the functions still declared, closed
  over what those objects call, are linked; the others are removed from `<object stem>.std/`.
  `VELT_STD_OBJECT_STATS=1` prints the reasons (`inlinable` for the functions not marked), then
  `velt: std cache: <n> found, <m> compiled, <k> left in the program`, `velt: std cache: <n> found
  without whole-program attributes` when any were, each `velt: std cache: links <symbol> (<n>
  statements)` as its function becomes a declaration (`<n>`: its body as velt_opt left it, inlined
  into as the budget allowed but with no per-function pass after the first, so larger than its
  optimized code), and
  `velt: std cache: <n> no longer called`. The stamp's `outside` line is worked out before functions become declarations
  when one does, else after optimization, as without the cache.
- The launcher (additive, #948; crates/velt_launcher, binary `velt-launcher`, installed as
  `<root>/bin/velt[.exe]`; `<root>` is the launcher's grandparent directory, else it exits 1).
  A first argument `+<toolchain>` is removed and selects that toolchain (reason `+<toolchain>`);
  a toolchain's own `velt` given one exits 1 saying it is the launcher's. For every command but
  `toolchain` it selects `+<toolchain>`, else `$VELT_TOOLCHAIN` (both: a full version is that
  version, a `[a-z]…` name a link, anything else a requirement read like a pin),
  else the `velt` requirement of the nearest `package.velt` above the cwd
  (`velt_toolchain::pin::find_pin`; a manifest that does not parse counts as no pin): the
  version the `velt` field of the `velt.lock.json` beside it records, as an exact toolchain
  (reason `velt.lock.json, with velt: "<req>" in <manifest>:<line>`), when the requirement
  accepts it and the command is not `update` (`velt_toolchain::pin::locked_version`; a missing
  or unreadable lock or field counts as none); else the requirement, else
  `<root>/default`; none → exit 1 naming `velt toolchain install`. A requirement selects the
  newest installed version it accepts (`<root>/toolchains/<v>/` with a `bin/velt`). When none
  is installed: with `$VELT_TOOLCHAIN_AUTO_INSTALL` `0`/`false`/`no`/`off`, exit 1 naming
  `velt toolchain install <requirement or version>`; else an exact version, or the newest
  release of the signed index `$VELT_INSTALL_BASE_URL/releases/download/index/releases.json`
  (`velt_toolchain::release`: format 1, checked against `releases.json.sig` with the release
  key) that the requirement accepts and that is not yanked (an exact requirement may name a
  yanked one; none → exit 1 listing the published versions that are not yanked), is installed
  (`velt_toolchain::release::install_toolchain`: `SHA256SUMS` then
  `velt-<v>-<host>.tar.gz` of `releases/download/v<v>/`, checked, unpacked under
  `velt-<v>-<host>/`, swapped into place) after `velt: installing velt <v> (<reason>) from <base>`
  on stderr. It then runs `<prefix>/bin/velt` with the same arguments (Unix: `exec`; Windows:
  waits, exits with its code, ignores Ctrl-C itself, and puts it in a kill-on-close job object
  so killing the launcher ends it), unless that file is the launcher itself (exit 1, "is this
  launcher") or `$VELT_LAUNCHER_HOPS` (incremented for the child; a toolchain's `velt` clears
  it) has reached 3 (exit 1: launchers started one another), with `VELT_LAUNCHER=<launcher path>` and
  `VELT_TOOLCHAIN_SELECTED=<toolchain> (<reason>)` set; `<reason>` is `$VELT_TOOLCHAIN`,
  `velt: "<req>" in <manifest>:<line>`, `+<toolchain>` or `the default`. `velt doctor` reports both
  (`toolchain`), or "started directly". `velt toolchain` in a toolchain's own `velt` exits 1
  saying it is the launcher's. After installing a version (on first use or `velt toolchain install`), the launcher runs that
  toolchain's `velt target add <triple>`, once per pack and with its stdout sent to stderr, for
  the target packs of the newest installed older version of the same `major.minor`
  (`velt_toolchain::layout::predecessor`, `installed_target_packs`:
  `lib/targets/<triple>/kit.stamp` whose name is `[a-z0-9_.-]+` not starting with `-` or `.`,
  the host's excluded), after `velt: adding the target packs velt <old> had to velt <new>: …` on
  stderr; the packs that fail are named in a warning with `velt +<new> target add …`, and the
  install succeeds. Before running a release version, the launcher prints `velt: <notice>` on stderr when the
  index it last fetched (`<root>/index-cache.json`, written by every verified fetch;
  `velt_toolchain::release::notice_for`) marks that version yanked or gives it an `advisory`
  (`security: true`: "has a security problem", also when it is yanked, the advisory after the
  yank reason), naming the newest release of the same minor with neither and `` (`velt update`
  in a package pinned to it, or `velt toolchain install <v>`) ``; at most once a day per version
  (`<root>/notices/<version>`). Yank reasons and advisories are printed through
  `velt_toolchain::release::display_text` (here and in `list --available`): control characters
  and bidi overrides (U+202A–202E, U+2066–2069) become `?`, line breaks and tabs a space, cut at
  200 characters with `…`. For `update` in a pinned package (the package's `velt` field, not
  `+<req>` or `$VELT_TOOLCHAIN`) it first takes the newest published version the requirement
  accepts from the index, installing it, unless an installed version it accepts is as new or
  newer (an unreachable index: a note, then as usual). `velt toolchain` (`velt_launcher::commands`): `list` prints a
  line per installed version (newest first) then per link (`<name> -> <prefix>`), each after two
  marker columns (`*` default, `>` selected here); `--available` prints the index's versions,
  newest first, `  (installed)` after installed ones and `  (yanked: <why>)` after yanked ones.
  Reading the index refuses one whose `generated` is older than `<root>/index-seen`, and prints
  `velt: a newer velt launcher (<v>) is available (this one is <own>): run \`velt toolchain self-update\`` on stderr when its `launcher` is newer than
  the running launcher. `install --from <file> [--unverified] [--default]`
  (`velt_toolchain::release::install_toolchain_from_file`): `<file>` must be named
  `velt-<semver>-<host>.tar.gz`; unless `--unverified` (a warning on stderr), `SHA256SUMS` and
  `SHA256SUMS.sig` beside it are checked with the root's trusted keys and the archive against
  `SHA256SUMS` (refused unless UTF-8); the archive's top directory must be `velt-<v>-<host>`; then
  as `install`. When `<v>` is installed already it is kept and the file is not read (stdout
  `velt <v> is already installed; kept it …`). `install <spec>`: a full version is exact,
  anything else a requirement resolved against the index; the first installed toolchain (no
  `<root>/default`) or `--default` becomes the default. `remove` refuses the default without
  `--force`. `self-update`: the index's `launcher` when newer than the running launcher (else
  `the velt launcher is up to date (<v>)`), its toolchain installed first when missing (as
  `install`), and that prefix's `bin/velt-launcher` copied to `<root>/bin/.velt-new-<pid>`, run
  there as `toolchain --version` (it must print `velt-launcher <new>`, else the copy is deleted and
  the error says `the launcher of velt <new> (<path>) does not work here: …; kept the current
  one`), and renamed over `<root>/bin/velt` (on Windows the running `velt.exe` is first renamed to
  `.velt.exe.old-<pid>`); prints `updated the velt launcher: <old> → <new>`. It refuses unless the
  running launcher is `<root>/bin/velt` itself (canonical paths). Every launcher run deletes
  `<root>/bin/.velt*.old-*`, and `.velt-new-*` older than an hour. `prune` removes every installed version but the default, the version the cwd
  selects (`select` then `installed`), those whose directory is a link's prefix or contains it
  (`Root::linked_versions`), and per `major.minor` its newest release (its newest pre-release
  when it has none; `velt_toolchain::layout::prunable`), printing `removed velt <v> (<n> MB)` per
  version and `freed <n> MB` (`--dry-run`: `would remove` / `would free`, nothing removed; none
  to remove: `nothing to prune: …`); a version that cannot be removed (in use) is reported and
  the others still go, and the exit status is then 1. `default <t>` requires it installed. `which` prints `<toolchain> (<reason>)` and
  the prefix, or `<wanted> (<reason>): not installed; …`. `link <name> <prefix>` (`name`:
  `[a-z][a-z0-9._-]*`) requires `<prefix>/bin/velt` and writes the absolute prefix to
  `<root>/links/<name>`.
- `--emit vir` prints VIR (`Display`) to stdout and stops. `-v` prints per-stage timings;
  `--timings` adds each optimizer pass and, with the LLVM backend, IR printing and clang (with
  Cranelift: `declare`, `functions`, `define`, `object`; the program is verified once after
  optimizing, not again in codegen), and each stage's line ends with the compiler's memory then
  (`(rss N MB, peak M MB)`). Debug builds run VIR verification's per-function checks, the
  optimizer's per-function passes and Cranelift's translation and compilation on
  `$VELT_JOBS` threads (a positive number, at most 4 per core; default: the available parallelism), summing a pass's
  time over the threads; the object is the same bytes on any number of threads.
- Debug info: debug builds always carry it; `-g` keeps it in a `--release` build (and links with
  debug settings: PDB on Windows, no `-s` strip on Linux). With the LLVM backend it is full line
  info (`.velt` file:line in debuggers and symbolizers); with Cranelift, DWARF line tables in ELF,
  Mach-O and COFF objects (the bundled `lld-link` keeps them in the `.exe`, beside the PDB; a
  build that `link.exe` links emits none, leaving function symbols).
- Panics print `panic: <msg> at <path>:<line>:<col>` (path as given to the compiler; a
  standard-library file as `std/<path below the std root>`) and exit 101;
  an uncaught error prints `Uncaught <Type>[: <message>] at <throw location>` and exits 1.
- `run` builds then executes the program with inherited stdio and **exits with the program's exit
  code**. Compiler errors → exit code 1, nothing executed. Internal compiler errors → 101.
- Diagnostics go to stderr as `Diagnostic::render` output.
- `dev` (docs/internals/design/hot-reload.md, phases 1–2): builds and runs like `run`, then stays up as a
  supervisor. It watches every file the build read (std and path dependencies included) plus
  `package.velt`/`velt.lock.json`, and new source files (`.velt`, `.ts`, `.tsx`) in their directories (OS file notifications,
  checked against mtime and length; polling every 10 ms where notifications fail or with
  `VELT_DEV_POLL=1`; 30 ms settle). On a change it builds the new version
  while the old one keeps running: a failed build prints its diagnostics and
  `velt dev: build failed (the previous version keeps running); waiting for changes`; a good one
  stops the old version (a stop request: SIGTERM on Unix, `stop` on the dev channel on Windows;
  drain up to 1 s, then kill) and starts the new one: `velt dev: reloaded in <n> ms` (from the
  first file change). A program that exits prints
  `velt dev: program exited with code <n>; waiting for changes`. Status lines go to stderr; the
  program's stdio is inherited. Runs until interrupted: Ctrl-C, SIGTERM or SIGHUP (console
  events on Windows) stop the program like a reload does, wait for it, and exit with
  128 + the signal (130 on Windows); a second interrupt exits at once. On Windows programs also
  run in a job object that ends them with the supervisor.
  - Default mode (every platform): each version is a `velt dev --host` child (**internal**, not for
    users) that compiles to VIR, JIT-compiles it with Cranelift (debug settings, no link, no new
    executable) and runs it in-process with the runtime linked into `velt`; it reports its build
    over the dev channel and starts only after the old version stopped (rt_abi_async.md §13).
    `--timings` (additive) breaks its `jit` stage into `compile`, `finalize`, `unwind` and
    `debug info`; `VELT_DEV_DEBUG_INFO=0` (additive) skips the debug-info image it registers for
    debuggers.
  - Hot swap (default mode, docs/internals/design/hot-reload.md phase 3): while a host runs, a change goes
    to it first. It builds the new version beside the running program and swaps the changed
    functions in, so in-memory state, open connections and running tasks survive: new calls and
    requests run the new code, work already in flight finishes on the old code. Printed:
    `velt dev: hot-swapped <n> function(s) in <ms> ms`. Edits that live state could not survive
    (a struct/class layout or a closure's captures changed, a function's signature changed,
    `main` changed, a function live values may still call was removed, closures were reordered)
    start a new host instead: `velt dev: restarted (<reason>) in <ms> ms`, e.g.
    `restarted (Point gained a field)`. A failed build keeps the program running, as above. A
    host that dies (or stops answering) while it takes a change is replaced by a new host
    running the change, and said so (additive): `velt dev: the running program crashed during
    the reload (<how>); restarted it in <ms> ms`, `<how>` being `exit code 0x<NTSTATUS>` on
    Windows or `signal <n>` on Unix (`exited during the reload (exit code <n>)` and `stopped
    answering during the reload` for the other cases). On Windows x64 a host that faults with an
    access violation first prints where (faulting address, registers, stack) to stderr.
  - `--exe`: each version is a linked debug executable with its own file
    (`<target dir>/dev/<stem>-<n>`, numbered per session), so a running (on Windows: locked)
    executable is never overwritten; a version's files are deleted once its process has exited,
    leftovers of an earlier session at the start. `--release`, `-g` and `--backend` are accepted
    only with `--exe`.
  - Listening sockets survive restarts: the supervisor owns them and hands them to each version
    (`VELT_DEV_SOCKET`), so no connection is refused during a reload and port 0 keeps its port.
- `test --watch`: runs the tests, then again after every change to a file the test builds read,
  a test file or the manifest/lockfile (each run discovers test files anew). Runs until
  interrupted.
- `test`: finds `*.test.velt`, `*.test.ts` and `*.test.tsx` (recursively, skipping `target/`,
  `node_modules/`, hidden and symlinked directories and nested packages below the searched
  directory); every `export function test_*()` (no
  params) is a test (additive: `export async function test_*()` too; the harness awaits it).
  Prints `ok <name>` / `FAILED <name>` and a summary; exit 1 on any failure. Test binaries in
  `<pkg or cwd>/target/velt/test/`.
- `new` creates `<name>/package.velt`, `src/main.velt` (or `src/lib.velt` with `--lib`), `.gitignore`.
- **Templates** (additive, tooling): `new --template <t>` (default `app`; `--lib` = `--template
  lib`) also writes `README.md` and `tests/*.test.velt`; every template builds, passes `velt test`
  and is `velt fmt`-clean. `app`: hello world with a module and a test. `cli`: std/cli argument
  parsing, subcommands, `--help`, usage errors → exit 2. `api`: JSON HTTP API (routes,
  validation, `ApiError` subclasses → status codes, `shared<Mutex<…>>` state), tests with `fetch`
  against a server on port 0. `websocket`: chat server + terminal client (`serve`/`connect`).
  `lib`: `src/lib.velt` exports with `/** */` doc comments for `velt doc`. Templates are embedded in the
  binary (`crates/veltc/templates/`); `{{name}}` in them becomes the package name.
- `init` (additive): the same files in the current directory; the package is named after the
  directory (lower-cased, other characters → `-`) unless `--name`. Files the template would
  create that already exist are a conflict: nothing is written and the error lists them, unless
  `--force` (overwrites them). An existing `README.md` is always kept; `target/` is appended to an
  existing `.gitignore`. Refuses (without `--force`) inside another package.
- `clean` (additive): removes `<package>/target` (found upward from the cwd like `build`) and
  prints `Removed <dir> (<n> files, <size>)`; no `target/` → `Clean nothing to remove`.
  `clean --cache` removes the build cache's directory instead (same output; the cache turned off
  → `Clean nothing to remove (the build cache is off: $VELT_BUILD_CACHE)`).
- `cache warm [--background]` (additive, #1612): with the build cache on, builds each
  `*.velt` of the warm-up directory (`$VELT_SHARE/warm`, else `share/velt/warm` beside the std
  root's directory: `<prefix>/share/velt/warm`, or the checkout's) with a child `velt build
  <file> -o <tmp>/warm[.exe]` (an executable; `VELT_BUILD_STAMP=off`, `VELT_STD_OBJECT_FILL=1`, no `VELT_CPU`), once with `--cpu native` and once without, and
  with clang found the same again with `--release` (no `VELT_STD_OBJECT`; a build with
  `VELT_STD_OBJECT_FILL` stores no LLVM codegen units, only `stdfn` entries), then
  writes the marker `<cache>/warm/<hex>` (BLAKE3 of the version line and the executable's size
  and time; `<hex>-objects` with clang found, whose builds store objects) and prints `velt: warmed the build cache with <n> programs in <s>s`. With the marker
  present it prints that the cache is warm and does nothing; one run per marker at a time
  (`<marker>.lock`). A program that fails to build is reported (exit 1) after the others ran; the
  marker is still written, naming the failed programs, unless every program failed. `--cpu
  native` is keyed as the word, not the host's CPU: a cache shared between machines (copied, or on
  a network) can link objects another machine built for its `native`, as the LLVM unit cache can
  for `-march=native`. `--background` starts `velt cache warm` detached (own process group, lowest
  priority; on Windows `CREATE_NO_WINDOW`, `IDLE_PRIORITY_CLASS`) and returns; it does nothing
  when `$CI` is set (non-empty) or `$VELT_CACHE_WARM` is `off`. The cache off →
  a note, exit 0; no warm-up directory → error. The launcher runs `velt cache warm --background`
  after installing a toolchain, as do `scripts/install.*` and `scripts/get-velt.*` (ignoring
  failures).
- `completions <shell>` (additive): prints a completion script for bash, zsh, fish or PowerShell
  (`pwsh` accepted) covering commands, options, `--template`/`--backend`/`--emit` values and files.
- Help (additive): `velt --help` lists commands, templates and environment variables;
  `velt <command> --help` / `-h` (anywhere before `--`) and `velt help <command>` print usage,
  options and examples, exit 0. Usage errors exit 2 and end with
  `For more information, try `velt <command> --help`.`; an unknown command or option names the
  closest match (`did you mean `velt build`?`).
- Colors (additive): `error:`/`warning:`, status verbs, help headings and test `ok`/`FAILED` are
  colored when the stream is a terminal, unless `NO_COLOR` is set (non-empty) or `TERM=dumb`;
  `CLICOLOR_FORCE=1` forces colors.
- Errors for common mistakes (additive): a package command outside any package names the
  `.velt` files in the cwd (`velt run <file>`) and `velt init`; a missing input file suggests
  `<name>.velt` or a similarly named file; a directory input says to build the package inside it.
- `add` edits `dependencies` in `package.velt`, keeping comments, then formats the file (latest published version if no req) and installs.
- `manifest [--json]` (additive) reads and validates `package.velt` (manifest.md). Without flags
  it prints one `Checked <path> (<name> <version>)` status line on stderr; `--json` prints the
  manifest as pretty JSON on stdout with defaults filled in. A manifest error exits 1.
- `fmt` without paths in a package that has only a `velt.toml` reports the migration error
  (manifest.md) instead of formatting.
- `install` resolves + fetches deps and writes `velt.lock.json`; `--locked` fails if the lock would change.
  `update` re-resolves ignoring the lock. `publish` uploads the package to its registry (or copies it into a local directory registry).
- Native libraries (additive, native_abi.md): `build`, `run`, `check`, `dev`, `test` and the LSP
  install the packages' native libraries for the target (prebuilt and verified, or built with
  cargo for path packages and missing targets); `add`/`install`/`update` print one
  `Native: `<pkg>` <ver> runs native code (prebuilt, checksum verified, <triple>)` (or
  `built from source`) line per such package. `native build` writes the current package's bundle
  to `<pkg>/target/velt-native/<triple>/` (default: the host). `publish` adds a bundle for every
  `native.targets` entry, from `--native-artifacts <dir>/<triple>/` or
  `target/velt-native/<triple>/` (the host's is (re)built), and fails if one is missing;
  `--native-only` adds bundles for targets not yet published to the published version.
- `fmt` formats in place (no paths: the package's `package.velt` and the `.velt`/`.ts`/`.tsx`
  files of `src/`, or all `.velt` under cwd outside a package; a directory path: its `.velt`
  files, plus `.ts`/`.tsx` (not `.d.ts`) under a package's `src/` and `tests/`; skips
  `target/`, `node_modules/`, hidden dirs). `--check` writes nothing, lists unformatted files,
  exit 1 if any. Unparsable files → exit 1.
- Imports: `velt/x` → `<std root>/x.velt` or `x/index.velt`; `./x`, `../x` → relative `x.velt`,
  `x.ts` or `x.tsx`, else folder module `x/index.velt`, `x/index.ts` or `x/index.tsx` (two
  existing files among one of these triples: an error at the specifier naming them); `./x.velt`,
  `./x.ts`, `./x.tsx` → exactly that file; `./x.js` → `x.ts` or `x.tsx`, `./x.jsx` → `x.tsx`
  (TypeScript's resolution). Module paths drop the extension (`x.ts` → `x`). JSX in a `.ts`
  module is an error (jsx.md). Bare names → `paths` aliases of the importing package first
  (the target resolved like a relative path), then packages via `package.velt` (`pkg` →
  `src/lib.velt`, `.ts` or `.tsx`; `pkg/sub` → `src/sub.*` or `src/sub/index.*` with the same
  three extensions; a bare specifier with a source extension is an error). The names a
  specifier spells must match the disk in case on every OS (else an error naming the file on
  disk and the corrected specifier); a file module that hides a folder module whose `index` has
  another extension gets a warning. Root files may be `.ts` or `.tsx` too; a package without
  `entry` uses `src/main.velt`, `.ts` or `.tsx` (`src/lib.*` for a library). `std/prelude/*.velt` is loaded implicitly before everything else.
  Global modules in `std/prelude/global/*.velt` (`fetch`, `Response`, `URL`, `JSON`, …) join the prelude
  only when a non-std module mentions one of their names as a whole word, unless that module
  binds the name itself at the top level.
- Environment: `VELT_STD` (std root), `VELT_HOME` (default `~/.velt`), `VELT_REGISTRY`
  (an `http(s)://` URL or a directory; default the package's `registry`, else
  https://registry.veltpm.com), `VELT_RT_LIB` (runtime lib), `VELT_RT_LINK` (`static`: no shared runtime in debug builds), `VELT_LINKER` (`bundled`, `system`, or a linker program), `VELT_CLANG` (clang for the LLVM backend), `VELT_CLANG_PROBE_LIMIT_MS` (how long a clang gets to answer `--version` or the small compile, default 30000: for tests of a clang that hangs), `VELT_LLVM_OPT` (clang `-O` level of release builds, default 3), `VELT_CODEGEN_UNITS` (codegen units of LLVM release builds; default from the program's size), `VELT_BUILD_CACHE`, `VELT_BUILD_CACHE_SIZE`, `VELT_BUILD_STAMP` (above), `VELT_EAGER_STD` (set: load up front the standard-library modules otherwise loaded when the check finds them needed; the program built is byte for byte the same), `VELT_CHECK_ALL_STD` (set: check every standard-library definition the program loads, not only those it reaches, and load as `VELT_EAGER_STD` does; docs/internals/design/lazy-std.md).
  Set by `velt dev` for the program (not for users): `VELT_DEV_SOCKET` (a Unix socket path, or a
  named pipe `\\.\pipe\velt-dev-<pid>-<n>` on Windows).
- Lockfile: `version = 1` + `[[package]]` entries with `name`, `version`, `source`
  (`"registry"` | `"path+<rel>"`), `checksum`, `dependencies`.
