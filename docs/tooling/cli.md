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
| `velt dev` | run, then hot-swap or restart on every change ([`velt dev`](dev.md)) |
| `velt test` | run the tests ([Testing](../book/testing.md)) |
| `velt fmt` | format `.vlt` files ([Formatter](fmt.md)) |
| `velt clean` | remove the package's `target/` directory |
| `velt add`, `install`, `update`, `publish` | packages ([Packages](packages.md)) |
| `velt doc` | generate HTML API documentation |
| `velt lsp` | the language server ([Editors](editors.md)) |
| `velt playground` | write and run programs in the browser ([WebAssembly](webassembly.md#the-playground)) |
| `velt registry serve` | serve a package registry over HTTP ([Packages](packages.md#registries)) |
| `velt doctor` | check the installation and run a hello world |
| `velt completions <shell>` | print a completion script for bash, zsh, fish or PowerShell |

## Files and packages

Every build command works on a single file or on a package:

- **A file**: `velt run hello.vlt` builds `./target/velt/hello` (`hello.exe` on Windows),
  relative to the current directory, and runs it. If the file is inside a package, the package's
  dependencies are installed first.
- **A package**: without a file argument, `velt` searches upward from the current directory for
  `velt.toml` and builds the package's entry (default `src/main.vlt`) to
  `<package>/target/velt/<name>`. A library-only package can't be run.

## `velt build` and `velt run`

```
velt build [<file.vlt>] [-o <out>] [--release] [-g] [--target <triple>] [--backend cranelift|llvm]
           [--emit vir|llvm|obj|exe] [--locked] [-v] [--timings]
velt run   [<file.vlt>] [--release] [-g] [--target <triple>] [--backend cranelift|llvm]
           [--locked] [-v] [-- <program args>...]
```

- **Debug builds** (the default) use Cranelift: fast to compile, with function symbols for
  backtraces.
- **`--release`** runs Velt's optimizer, then LLVM `-O3` when clang 16 or newer is found
  (`VELT_CLANG`, `PATH`, the standard install directories); otherwise Cranelift, with a
  one-line note. `-g` keeps debug info in a release build ([Debugging](debugging.md)).
- `--backend llvm` without `--release` gives an unoptimized build with full line information.
- `--target wasm32-wasip1` and `--target wasm32-unknown-unknown` build WebAssembly
  ([WebAssembly](webassembly.md)); `--target x86_64-apple-darwin` cross-builds on Apple
  silicon.
- `--emit vir` / `--emit llvm` print the intermediate representation and stop; `--emit obj`
  writes only the object file.
- `-v` prints per-stage timings; `--timings` adds each optimizer pass and code generation step.
- `run` exits with the program's exit code and passes arguments after `--` to the program.
  Compile errors exit with 1 and run nothing.

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

Generates HTML documentation for exported items: their signatures as written and the `///`
comment block right above each declaration (a comment block at the top of a file documents the
module). Without paths, it documents the package's `src/` into `<package>/target/doc`;
`--std` documents the standard library. The output has one page per module and a client-side
search.

## `velt doctor`

Checks the runtime library, the standard library, the system linker, clang, and that
`VELT_HOME` is writable, then compiles and runs a hello world (debug, plus release through LLVM
when clang is found). Problems are marked `✗` (required) or `!` (optional) with a `fix:` hint.
It exits with 0 when every required check passes.

```
$ velt doctor
✓ velt             velt 0.1.0 (4efaa8d x86_64-pc-windows-msvc)
✓ runtime lib      C:\Users\me\AppData\Local\velt\lib\velt_rt.lib
✓ std              C:\Users\me\AppData\Local\velt\std
✓ linker           C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\...\link.exe
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
| `VELT_REGISTRY_TOKEN` | token for `velt publish` to a registry server, and the token a server accepts |
| `VELT_CLANG` | clang for the LLVM backend |
| `VELT_RT_LIB` | runtime library (default: next to `velt`, or `<prefix>/lib` when installed) |
| `VELT_LINKER` | linker override |
| `VELT_LLVM_BIN` | directory with LLVM's `opt` and `llc`, for WebAssembly |
| `VELT_WASI_SYSROOT` | wasi-libc directory, for `wasm32-wasip1` |
| `VELT_WASM_RUNNER` | program that runs `wasm32-wasip1` modules for `velt run` (default: wasmtime) |
| `VELT_THREADS` | number of runtime worker threads (default: one per core) |
| `NO_COLOR` | disable colored output |
