# The `velt` command

One binary is the whole toolchain: compiler, runner, test runner, formatter, package manager,
language server and documentation generator. `velt <command> --help` (or `velt help <command>`)
prints every command's options and examples.

| Command | What it does |
|---|---|
| `velt new <name>` | create a package in a new directory from a template |
| `velt init` | turn the current directory into a package |
| `velt build` | compile a file or the current package |
| `velt run` | build and run a file or the current package |
| `velt check [--json]` | type-check a file or the current package without building it ([`velt check`](#velt-check)); `--ts-compat` lints code shared with TypeScript ([below](#code-shared-with-typescript---ts-compat)) |
| `velt dev` | run, then hot-swap or restart on every change ([`velt dev`](dev.md)) |
| `velt test` | run the tests ([Testing](../book/testing.md)) |
| `velt fmt` | format `.vlt` (and `.ts`, `.tsx`) files ([Formatter](fmt.md)) |
| `velt clean` | remove the package's `target/` directory |
| `velt add`, `install`, `update`, `publish` | packages ([Packages](packages.md)) |
| `velt manifest [--json]` | check the package's manifest, or print it as JSON for other tools ([`package.vlt`](manifest.md#other-tools)) |
| `velt search`, `yank`, `owner` | find and manage published packages ([Registries](packages.md#registries)) |
| `velt login`, `logout` | store or forget your token for a registry server ([Users, owners and yanking](packages.md#users-owners-and-yanking)) |
| `velt doc` | generate HTML API documentation |
| `velt lsp` | the language server ([Editors](editors.md)) |
| `velt playground` | write and run programs in the browser ([WebAssembly](webassembly.md#the-playground)) |
| `velt registry serve`, `registry user`, `registry owner` | serve a package registry over HTTP, and manage its users and owners ([Packages](packages.md#registries)) |
| `velt doctor` | check the installation and run a hello world |
| `velt completions <shell>` | print a completion script for bash, zsh, fish or PowerShell |

## Files and packages

Every build command works on a single file or on a package:

- **A file**: `velt run hello.vlt` builds `./target/velt/hello` (`hello.exe` on Windows),
  relative to the current directory, and runs it. The file, and the modules it imports, may also
  be `.ts` or `.tsx` files ([TypeScript files](../reference/modules.md#typescript-files-ts-and-tsx)). If the file is inside a package, the package's
  dependencies are installed first.
- **A package**: without a file argument, `velt` searches upward from the current directory for
  `package.vlt` and builds the package's entry (default `src/main.vlt`) to
  `<package>/target/velt/<name>`. A library-only package can't be run.

## `velt build` and `velt run`

```
velt build [<file.vlt>] [-o <out>] [--release] [-g] [--target <triple>] [--backend cranelift|llvm]
           [--emit vir|llvm|obj|exe] [--locked] [-v] [--timings]
velt run   [<file.vlt>] [--release] [-g] [--target <triple>] [--backend cranelift|llvm]
           [--locked] [-v] [-- <program args>...]
```

- **Debug builds** (the default) use Cranelift: fast to compile, with line tables for debuggers
  on Linux (macOS untested; function symbols on Windows).
- **`--release`** runs Velt's optimizer, then LLVM `-O3` when clang 16 or newer is found
  (`VELT_CLANG`, `PATH`, the standard install directories); otherwise Cranelift, with a
  one-line note. `-g` keeps debug info in a release build ([Debugging](debugging.md)).
  An LLVM build (`--release`, or `--backend llvm`) of a large program (above about 32 000 VIR
  statements) is split into codegen units that clang compiles in parallel, one object file
  each, which builds it several times faster on a multi-core machine. Functions that call each
  other, and small helpers, stay in their caller's unit, so in the
  [measurements](../../bench/RESULTS.md) split programs run as fast as unsplit ones (within
  ±3 %, one faster). Smaller programs are one unit. `VELT_CODEGEN_UNITS=N`
  asks for N units (at most the core count); `VELT_CODEGEN_UNITS=1` turns splitting off.
- `--backend llvm` without `--release` gives an unoptimized build with full line information.
- `--target wasm32-wasip1` and `--target wasm32-unknown-unknown` build WebAssembly
  ([WebAssembly](webassembly.md)); `--target x86_64-apple-darwin` cross-builds on Apple
  silicon.
- `--emit vir` / `--emit llvm` print the intermediate representation and stop; `--emit obj`
  writes only the object file.
- `-v` prints per-stage timings; `--timings` adds each optimizer pass and code generation step.
- `run` exits with the program's exit code and passes arguments after `--` to the program.
  Compile errors exit with 1 and run nothing.

## `velt check`

```
velt check [<file.vlt>] [--json] [--locked] [-v]
velt check --ts-compat [<file|dir>...] [--json] [--locked] [-v]
```

Parses and type-checks a file with every file it imports, or the whole current package, and prints
the diagnostics like `velt build`, but builds nothing, so it answers in tens of milliseconds.
It exits with 0 when there are no errors (warnings are allowed) and with 1 when there are.

- A **library module** needs no `main`: `velt check lib.vlt` checks every function in it,
  including exported functions nothing calls. `velt build` and `velt run` still require `main`.
- In a package, `velt check` without a file checks the whole package, like `tsc` checks a
  project: every `.vlt`, `.ts` and `.tsx` module under `src/` and `tests/` (recursively,
  skipping `target/`, `node_modules/`, hidden and symlinked directories; `.d.ts` files are not
  modules), including `src/lib.vlt` next to `src/main.vlt`, modules nothing imports, and test
  files. The entry (`package.entry`, default `src/main.vlt`) must define a valid `main`; every
  other module is checked as a library module. A library package (no configured entry and no
  `src/main.vlt`) checks `src/lib.vlt` and the rest the same way. All modules are checked
  together, so a module several of them import is checked, and its errors reported, once. Two
  files in one directory whose names differ only in the extension (`src/dup.vlt` and
  `src/dup.ts`) are an error: an import can't tell them apart. Other
  directories (`examples/`, `bench/`, scripts next to `package.vlt`) usually hold programs of
  their own: check them with `velt check <file>`.
- `velt check <file>` checks that file and the files it imports, and nothing else.
- `--json` prints one JSON document on stdout instead, for editors and other tools:
  `{"diagnostics": [...], "errors": n, "warnings": n}`, each diagnostic with its `severity`,
  `message`, `location` (`file`, 1-based `line`/`column`, `endLine`/`endColumn`), further
  `labels` and `notes`, plus `code` and `fix` (`null` except for `--ts-compat` findings).
- `-v` prints per-stage timings.

### Code shared with TypeScript: `--ts-compat`

A file in the common subset of TypeScript and Velt compiles with both `tsc` and `velt` and
behaves the same under both, so a client and a Velt server can share models, validation and
components. `velt check --ts-compat` checks exactly the files you pass (for a directory, its
`.vlt`, `.ts` and `.tsx` files, found as for a package), then reports what in them `tsc` would
reject or run differently. Without paths, it lints the folders the package's `tsCompat` lists
in [package.vlt](manifest.md#tscompat):

```text
$ velt check --ts-compat src/models
src/models/user.ts:3:14: error: `f64` is not a TypeScript type
  = note: `f64` is Velt's other name for `number`, the only name TypeScript knows
  = note: write `number`
  = note: ts-compat(velt-number-type)
```

- `velt check --ts-compat` with no paths, inside a package, lints every `.vlt`, `.ts` and
  `.tsx` file in its `tsCompat` folders, named from the current directory. Outside a package,
  or in one without `tsCompat`, it fails and says to list the folders or name the paths; so does
  a listed folder that doesn't exist, or folders without any source file. Explicit paths ignore
  `tsCompat`.
- Plain `velt check` (without `--ts-compat`) never lints, not even the `tsCompat` folders:
  the lint is opt-in, so a package check stays the compiler's verdict and CI decides where the
  TypeScript rules apply. The editor shows the findings live
  ([editors](editors.md#code-shared-with-typescript)).
- The files are checked first, together, as library modules; a file with errors of its own
  reports them and isn't linted. They must all be in one package (or none in a package): lint
  one package per run. Two files with the same module path (`dup.vlt` and `dup.ts`) are an
  error, as in a package check; a declaration file (`.d.ts`) can't be passed.
- Every finding says what TypeScript does, why Velt differs and what to write, and ends with a
  note naming its rule, `ts-compat(<code>)`. With `--json`, each finding also carries its `code`
  and, when the replacement is mechanical, a `fix`: `{"location", "replacement", "title"}`.
- A relative import must stay among the files passed (or in the `tsCompat` folders): `tsc`
  compiles every file a shared file imports.
- It exits with 1 when there is an error, from the check or from the lint.
- The rules and the subset are listed in
  [the TSX design](../internals/design/tsx.md#the-common-subset). `velt build` never runs the
  lint.

## `velt new` and `velt init`

```
velt new <name> [--template app|cli|api|websocket|lib] [--lib]
velt init [--template <t>] [--name <name>] [--force]
```

| Template | What you get |
|---|---|
| `app` (default) | hello world split into a module, with a test |
| `cli` | a command-line tool: `velt:cli` argument parsing, subcommands, `--help`, exit codes, tests |
| `api` | a JSON HTTP API: routes, validation, typed errors as status codes, tests with `fetch` against a live server |
| `websocket` | a WebSocket chat server and terminal client, with an end-to-end test |
| `lib` | a library: exports with `///` docs for `velt doc`, tests, ready for `velt publish` |

Every template builds, passes `velt test` and is formatted. `velt init` writes the same files
into the current directory; it never overwrites files without `--force`, always keeps an
existing `README.md`, and names the package after the directory unless `--name` is given.

## `velt doc`

```
velt doc [<file|dir>...] [--std] [-o <dir>]
```

Generates HTML documentation for exported items: their signatures and the `///` comment block
right above each declaration (a comment block at the top of a file documents the module).
Without paths, it documents the package's `src/` (`.vlt`, `.ts` and `.tsx` files) into
`<package>/target/doc`; `--std` documents
the standard library. The output has one page per module and a client-side search.

- **Signatures** are shown in one canonical form whatever the source's layout:
  `function pick<T extends Comparable<T>, U>(items: T[], limit?: i64): U | null`, with
  `static`, `async`, `get`/`set` on members and the type's `extends`/`implements`.
- **Type names link** to their documentation: types the module declares, imports (also
  `ns.Type` through `import * as ns`) or re-exports, among the modules documented together.
  Prelude types link when the prelude is part of the docs (`--std`, the docs website); a
  package's docs don't include std, so its prelude types stay plain text. Type parameters and
  parameter names never link.
- **Re-exports** are documented under the re-exporting module: `export { x as y } from "…"`
  shows `x`'s documentation as `y`, `export * from "…"` every export the module doesn't
  declare or list itself, each with a link to where it is declared. A re-export from a module
  that isn't documented alongside (another package; std when documenting a package) is listed
  as one line. Names in a local `export { a, b as c };` list are documented too.

## `velt doctor`

Checks the runtime library, the standard library, the system linker, the WebAssembly linker
(the Rust toolchain's `rust-lld` when Rust is installed), clang, and that
`VELT_HOME` is writable, then compiles and runs a hello world (debug, plus release through LLVM
when clang is found). Problems are marked `✗` (required) or `!` (optional) with a `fix:` hint.
It exits with 0 when every required check passes.

```
$ velt doctor
✓ velt             velt 0.1.0 (4efaa8d x86_64-pc-windows-msvc)
✓ runtime lib      C:\Users\me\AppData\Local\velt\lib\velt_rt.lib
✓ std              C:\Users\me\AppData\Local\velt\std
✓ linker           C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\...\link.exe
✓ wasm linker      C:\Users\me\.rustup\toolchains\...\bin\rust-lld.exe
✓ clang            C:\Program Files\LLVM\bin\clang.exe
✓ velt home        registry C:\Users\me\.velt\registry, cache C:\Users\me\.velt\cache
✓ hello (debug)    built with Cranelift and ran
✓ hello (release)  built with LLVM and ran
```

## Shell completions

```sh
velt completions bash > ~/.local/share/bash-completion/completions/velt
velt completions zsh > ~/.zfunc/_velt                  # with ~/.zfunc on $fpath
velt completions fish > ~/.config/fish/completions/velt.fish
velt completions powershell >> $PROFILE                # PowerShell
```

## Output and exit codes

- Diagnostics go to stderr as `path:line:col: error: message` ([Diagnostics](../reference/diagnostics.md)).
- A program that panics prints `panic: <message> at <path>:<line>:<col>` and exits with 101; an
  uncaught error prints `Uncaught <Type>: <message> at <location>` and exits with 1.
- Usage errors exit with 2 and suggest the closest command or option
  (``did you mean `velt build`?``). An internal compiler error exits with 101.
- Colors are on when the output is a terminal. `NO_COLOR=1` turns them off, `CLICOLOR_FORCE=1`
  forces them.

## Environment variables

| Variable | Meaning |
|---|---|
| `VELT_STD` | standard library directory |
| `VELT_HOME` | package manager home (default `~/.velt`: `cache/`, `registry/`) |
| `VELT_REGISTRY` | package registry: a directory (default `$VELT_HOME/registry`) or an `http(s)://` URL |
| `VELT_REGISTRY_TOKEN` | a registry token sent to every registry server, overriding the tokens `velt login` stored (for CI) |
| `VELT_CA_FILE` | PEM file of extra CA certificates to trust for `https://` registries |
| `VELT_CLANG` | clang for the LLVM backend |
| `VELT_LLVM_OPT` | clang optimization level for release builds: `3` (default), `2`, `1`, `s` or `z` |
| `VELT_CODEGEN_UNITS` | how many codegen units (parallel clang processes) an LLVM build uses, at most the core count; `1` turns splitting off; default: from the program's size (one unit below about 32 000 VIR statements) |
| `VELT_RT_LIB` | runtime library (default: next to `velt`, or `<prefix>/lib` when installed) |
| `VELT_LINKER` | linker override |
| `VELT_LLVM_BIN` | directory with LLVM's `opt` and `llc`, for WebAssembly |
| `VELT_WASI_SYSROOT` | wasi-libc directory, for `wasm32-wasip1` |
| `VELT_WASM_RUNNER` | program that runs `wasm32-wasip1` modules for `velt run` (default: wasmtime) |
| `VELT_THREADS` | number of runtime worker threads (default: one per core) |
| `MACOSX_DEPLOYMENT_TARGET` | oldest macOS a program runs on (default and minimum: 11.0 on arm64, 10.12 on x86_64) |
| `NO_COLOR` | disable colored output |
