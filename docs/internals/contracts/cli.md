# `velt` CLI — CONTRACT

```
velt build [<file.vlt>] [-o <out>] [--release] [-g] [--backend cranelift|llvm] [--target <triple>] [--emit vir|llvm|obj|exe] [--locked] [-v] [--timings]
velt run   [<file.vlt>] [--release] [-g] [--target <triple>] [--backend cranelift|llvm] [--locked] [-- <program args>...]
velt check [<file.vlt>] [--json] [--locked] [-v]
velt check --ts-compat [<file|dir>...] [--json] [--locked] [-v]
velt dev   [<file.vlt>] [--exe] [--locked] [-v] [--timings] [-- <program args>...]
velt test  [<file|dir>] [--release] [--locked] [--watch]
velt new   <name> [--template app|cli|api|websocket|lib] [--lib]
velt init  [--template <t>] [--name <name>] [--force]
velt clean
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
velt doc [<file|dir>...] [--std] [-o <dir>]     # HTML API docs
velt registry serve [--dir <d>] [--port <n>] [--host <addr>]   # package registry server (default 127.0.0.1:8091)
velt doctor                            # checks toolchain setup + smoke test
velt toolchain list [--available] | install <v> [--default] | remove <t> [--force] | default [<t>] | which | link <name> <prefix> | unlink <name>   # the launcher's (#948)
velt --version                         # velt <ver> (<git hash> <host triple>)
```
- **Single file**: `build <file>` writes `./target/velt/<stem>` (`.exe` on Windows) relative to the
  current dir, object file next to it. If the file lives inside a package, its deps are installed first.
- **Package mode** (no file argument): finds `package.vlt` upward from the cwd, builds its entry, output
  `<pkg>/target/velt/<name>[.exe]`. Library-only packages can't be run.
- Backends: default `llvm` for `--release` when clang is found (`$VELT_CLANG`, PATH, standard install
  dirs), else `cranelift` (with a one-line stderr note). `--emit llvm` prints LLVM IR (no clang needed).
  Release builds run `velt_opt` (Speed) before either backend.
- Codegen units (additive): the LLVM backend splits a large program into units compiled by
  parallel clang processes, at most one per core (`velt_codegen_llvm::emit_objects_timed`); the
  first unit's object is `<exe>.o` (`.obj`), the others `<exe>.cgu<N>.o` beside it, and all are
  linked; objects of higher-numbered units from an earlier build are removed. With
  `$VELT_CODEGEN_UNITS` unset, the count depends on the program's size only (not on the
  machine): one unit below 32 000 VIR statements, else one per 16 000, at most 4.
  `$VELT_CODEGEN_UNITS` = N overrides it (N units, at most the core count; `1`: no split).
  `--emit obj` always writes one object and removes none.
- **WebAssembly** (additive): `--target wasm32-wasip1` (alias `wasm32-wasi`)
  and `--target wasm32-unknown-unknown` build `./target/velt/<stem>.wasm` (object `<stem>.o`) with
  the LLVM backend (always; `--backend cranelift` is an error) using LLVM's `opt`/`llc`
  (`$VELT_LLVM_BIN`, else rustup's `llvm-tools`), `wasm-ld` (`$VELT_LINKER`, rustup's `rust-lld`,
  `wasm-ld` on PATH) and `libvelt_rt_wasm.a` (`$VELT_RT_LIB`, else cargo's `target/<triple>/<profile>/`,
  else `<prefix>/lib/<triple>/`). WASI builds also need wasi-libc (`$VELT_WASI_SYSROOT`, else
  rustup's `wasm32-wasip1` target). `run` executes WASI modules with `$VELT_WASM_RUNNER <module>
  <args>` or `wasmtime run --dir=. <module> <args>`; browser builds also write the JS glue
  `velt_web.mjs` next to the module, and `run` executes them with `node velt_web.mjs <module>`.
  No TCP, HTTP server or child processes on WebAssembly (link error with a note); `fetch` links
  but rejects with `IoError` `ENOTSUP` (no network), so code that only builds `Response`s runs.
- `playground` (additive): serves a page where programs are edited, compiled
  on the server to `wasm32-unknown-unknown` (`POST /api/compile[?release=1]`, body = source →
  `200 application/wasm` or `422` diagnostics text with paths as `main.vlt`) and run in the
  browser (Web Worker + `velt_web.mjs`). Only `std/…` imports are accepted. Needs the browser
  runtime (`cargo build -p velt_rt_wasm --target wasm32-unknown-unknown`).
- `doc` (additive): HTML API docs from exported items, their public members,
  their signatures as written and the doc comment right above each declaration: a JSDoc
  `/** … */` comment or a block of `///` lines, with JSDoc tags (`@param`, `@returns`,
  `@throws`, `@example`, `@deprecated`, `@see`, `{@link}`); plain `//` is not a doc comment
  (`docs/internals/design/doc-comments.md`). A comment block at the top of a file, followed by a
  blank line, documents the module. No paths: the package's `src/`
  (its `.vlt`, `.ts` and `.tsx` files, not `.d.ts`; `src/lib.vlt` is named after the package) into `<pkg>/target/doc`; paths: those files and
  directories into `./target/doc`; `--std`: the standard library. `-o` overrides the output
  directory. Writes `index.html`, one page per module, and a client-side search index.
- `registry serve` (additive): serves a registry directory (default: the
  local registry) over HTTP (protocol in manifest.md "Remote registries"). `publish`
  uploads when the package's `registry` (or `$VELT_REGISTRY`) is a URL.
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
  `warning: `<name>` <version> is yanked (pinned by velt.lock.json)` for each yanked locked version.
- `yank <pkg>@<version> [--undo]` (additive): sets or clears the version's `yanked` flag in the
  package's registry (local, or remote: owners only). Resolution skips yanked versions unless
  `velt.lock.json` pins them; `add` without a version picks the newest stable version not yanked
  (the newest pre-release not yanked if there is no stable one).
- `owner list|add|remove <pkg> [<user>]` (additive): a package's owners on a registry server
  (`list` prints one per line on stdout); an error for a local registry.
- `login <registry-url>` / `logout <registry-url>` (additive): `login` reads one line from stdin
  (prompting on stderr when stdin is a terminal) and stores it as the token of that registry in
  `$VELT_HOME/credentials.json`: `{"registries": {"<url>": {"token": "…"}}}`, keys with the scheme
  and host lowercased and no trailing `/`, written atomically (temporary file, synced, renamed),
  mode 0600 on Unix, an owner-only protected DACL on Windows; the terminal doesn't echo the
  token. `logout` removes the entry. Writes to a registry server (`publish`, `yank`, `owner
  add|remove`) send `$VELT_REGISTRY_TOKEN` if set, else the token stored for exactly that
  registry, else none. A token is never sent (nor stored) for a plain `http://` URL whose host is
  not loopback (a host part with `@`, `?`, `#` or `\` is never loopback); that is an error.
- `search <text> [--json]` (additive): `name  version  description` lines on stdout (columns
  aligned; the description cut to the terminal's width with `…`, not cut when stdout is not a
  terminal) for the packages of the package's registry (or `$VELT_REGISTRY`) that match the text
  by name, keywords or description (ranking in manifest.md "Search"), each with the version `add`
  picks: the newest stable version not yanked (the newest pre-release not yanked if it has no
  stable one). `--json` prints the registry's answer (`{"packages": [...]}`) instead.
- `check` (additive): parse + sema, no lowering, codegen or link. Input resolution: with a file,
  that file and its imports (the file's package, if any, supplies dependencies); the root module
  need not define `main` (a library module; every function body is still checked), a `main` that
  is there is validated as for `build`, which, like `run`, still requires one. Without a file, in
  a package: every source module (`.vlt`, `.ts`, `.tsx`; not `.d.ts`) under `src/` and `tests/`
  (recursively, skipping `target/`, `node_modules/`, hidden and symlinked directories and nested
  packages, i.e. directories with their own manifest, whose `package.vlt` is never a module; no
  other directory; `vpm::sources::walks_into`), loaded together in one front-end run with the package's root module: the entry
  (`package.entry`, default `src/main.vlt`), which must define a valid `main`, or, when there is
  no configured entry and no `src/main.vlt`, `src/lib.vlt` as a library module. Every other
  module is a library module; one whose module path is taken or reserved (`src/std/x.vlt`, a
  `src/util.vlt` next to a dependency `util`) gets a name no import can write instead of an
  error (`build` does not load it). Files in one directory whose names differ only in the
  source extension (`src/dup.vlt`, `src/dup.ts`) are an error per group, located in the first
  file and naming all of them relative to the package root. A configured entry that is missing is an error naming it (as
  for `build`). Package dependencies are installed. Every diagnostic the front end reports
  appears once, even for a module several roots import (all files' syntax errors; if there are
  none, all type errors). Exit 0 without errors (warnings allowed), 1 with errors, 101 on an
  internal error.
  Diagnostics go to stderr as for `build`; `--json` prints instead one JSON document on stdout:
  `{"diagnostics": [{"severity": "error"|"warning"|"note", "message", "location", "labels":
  [{"location", "message"}], "notes": [string]}], "errors": n, "warnings": n}` where a
  `location` is `{"file", "line", "column", "endLine", "endColumn"}` (1-based; columns count
  bytes, as in the text output) or `null`. A non-source failure (unreadable input, broken
  package) is an error diagnostic with `location: null`. `-v` prints stage timings to stderr.
  Additive: every JSON diagnostic also has `"code"` and `"fix"`, both `null` except for
  `--ts-compat` findings.
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
  no `package.vlt` …"), when the manifest has no `tsCompat` ("package `<name>` has no `tsCompat`
  folders to lint …"), when a listed folder is not a directory ("package `<name>`: `tsCompat`
  folder `<dir>` does not exist" / "is not a folder"), or when the folders hold no source
  module. Plain `velt check` never lints, `tsCompat` or not.
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
  against `SHA256SUMS.sig` with the release key (`velt_toolchain::signature`;
  `$VELT_INSTALL_PUBLIC_KEY` replaces the built-in key); an unsigned release or a signature
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
  (`<exe>.link-stamp` beside it).
- The launcher (additive, #948; crates/velt_launcher, binary `velt-launcher`, installed as
  `<root>/bin/velt[.exe]`; `<root>` is the launcher's grandparent directory, else it exits 1).
  For every command but `toolchain` it selects `$VELT_TOOLCHAIN` (a version or a link name),
  else the `velt` requirement of the nearest `package.vlt` above the cwd
  (`velt_toolchain::pin::find_pin`; a manifest that does not parse counts as no pin), else
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
  waits, exits with its code, ignores Ctrl-C itself), with `VELT_LAUNCHER=<launcher path>` and
  `VELT_TOOLCHAIN_SELECTED=<toolchain> (<reason>)` set; `<reason>` is `$VELT_TOOLCHAIN`,
  `velt: "<req>" in <manifest>:<line>` or `the default`. `velt doctor` reports both
  (`toolchain`), or "started directly". `velt toolchain` in a toolchain's own `velt` exits 1
  saying it is the launcher's. `velt toolchain` (`velt_launcher::commands`): `list` prints a
  line per installed version (newest first) then per link (`<name> -> <prefix>`), each after two
  marker columns (`*` default, `>` selected here); `--available` prints the index's versions,
  newest first, `  (installed)` after installed ones and `  (yanked: <why>)` after yanked ones. `install <spec>`: a full version is exact,
  anything else a requirement resolved against the index; the first installed toolchain (no
  `<root>/default`) or `--default` becomes the default. `remove` refuses the default without
  `--force`. `default <t>` requires it installed. `which` prints `<toolchain> (<reason>)` and
  the prefix, or `<wanted> (<reason>): not installed; …`. `link <name> <prefix>` (`name`:
  `[a-z][a-z0-9._-]*`) requires `<prefix>/bin/velt` and writes the absolute prefix to
  `<root>/links/<name>`.
- `--emit vir` prints VIR (`Display`) to stdout and stops. `-v` prints per-stage timings;
  `--timings` adds each optimizer pass and, with the LLVM backend, IR printing and clang.
- Debug info: debug builds always carry it; `-g` keeps it in a `--release` build (and links with
  debug settings: PDB on Windows, no `-s` strip on Linux). With the LLVM backend it is full line
  info (`.vlt` file:line in debuggers and symbolizers); with Cranelift, line tables in ELF and
  Mach-O objects (checked on Linux; macOS untested) and function symbols on Windows.
- Panics print `panic: <msg> at <path>:<line>:<col>` (path as given to the compiler) and exit 101;
  an uncaught error prints `Uncaught <Type>[: <message>] at <throw location>` and exits 1.
- `run` builds then executes the program with inherited stdio and **exits with the program's exit
  code**. Compiler errors → exit code 1, nothing executed. Internal compiler errors → 101.
- Diagnostics go to stderr as `Diagnostic::render` output.
- `dev` (docs/internals/design/hot-reload.md, phases 1–2): builds and runs like `run`, then stays up as a
  supervisor. It watches every file the build read (std and path dependencies included) plus
  `package.vlt`/`velt.lock.json`, and new source files (`.vlt`, `.ts`, `.tsx`) in their directories (OS file notifications,
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
- `test`: finds `*.test.vlt`, `*.test.ts` and `*.test.tsx` (recursively, skipping `target/`,
  `node_modules/`, hidden and symlinked directories and nested packages below the searched
  directory); every `export function test_*()` (no
  params) is a test (additive: `export async function test_*()` too; the harness awaits it).
  Prints `ok <name>` / `FAILED <name>` and a summary; exit 1 on any failure. Test binaries in
  `<pkg or cwd>/target/velt/test/`.
- `new` creates `<name>/package.vlt`, `src/main.vlt` (or `src/lib.vlt` with `--lib`), `.gitignore`.
- **Templates** (additive, tooling): `new --template <t>` (default `app`; `--lib` = `--template
  lib`) also writes `README.md` and `tests/*.test.vlt`; every template builds, passes `velt test`
  and is `velt fmt`-clean. `app`: hello world with a module and a test. `cli`: std/cli argument
  parsing, subcommands, `--help`, usage errors → exit 2. `api`: JSON HTTP API (routes,
  validation, `ApiError` subclasses → status codes, `shared<Mutex<…>>` state), tests with `fetch`
  against a server on port 0. `websocket`: chat server + terminal client (`serve`/`connect`).
  `lib`: `src/lib.vlt` exports with `/** */` doc comments for `velt doc`. Templates are embedded in the
  binary (`crates/veltc/templates/`); `{{name}}` in them becomes the package name.
- `init` (additive): the same files in the current directory; the package is named after the
  directory (lower-cased, other characters → `-`) unless `--name`. Files the template would
  create that already exist are a conflict: nothing is written and the error lists them, unless
  `--force` (overwrites them). An existing `README.md` is always kept; `target/` is appended to an
  existing `.gitignore`. Refuses (without `--force`) inside another package.
- `clean` (additive): removes `<package>/target` (found upward from the cwd like `build`) and
  prints `Removed <dir> (<n> files, <size>)`; no `target/` → `Clean nothing to remove`.
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
  `.vlt` files in the cwd (`velt run <file>`) and `velt init`; a missing input file suggests
  `<name>.vlt` or a similarly named file; a directory input says to build the package inside it.
- `add` edits `dependencies` in `package.vlt`, keeping comments, then formats the file (latest published version if no req) and installs.
- `manifest [--json]` (additive) reads and validates `package.vlt` (manifest.md). Without flags
  it prints one `Checked <path> (<name> <version>)` status line on stderr; `--json` prints the
  manifest as pretty JSON on stdout with defaults filled in. A manifest error exits 1.
- `fmt` without paths in a package that has only a `velt.toml` reports the migration error
  (manifest.md) instead of formatting.
- `install` resolves + fetches deps and writes `velt.lock.json`; `--locked` fails if the lock would change.
  `update` re-resolves ignoring the lock. `publish` copies the package into the local registry.
- Native libraries (additive, native_abi.md): `build`, `run`, `check`, `dev`, `test` and the LSP
  install the packages' native libraries for the target (prebuilt and verified, or built with
  cargo for path packages and missing targets); `add`/`install`/`update` print one
  `Native: `<pkg>` <ver> runs native code (prebuilt, checksum verified, <triple>)` (or
  `built from source`) line per such package. `native build` writes the current package's bundle
  to `<pkg>/target/velt-native/<triple>/` (default: the host). `publish` adds a bundle for every
  `native.targets` entry, from `--native-artifacts <dir>/<triple>/` or
  `target/velt-native/<triple>/` (the host's is (re)built), and fails if one is missing;
  `--native-only` adds bundles for targets not yet published to the published version.
- `fmt` formats in place (no paths: the package's `package.vlt` and the `.vlt`/`.ts`/`.tsx`
  files of `src/`, or all `.vlt` under cwd outside a package; a directory path: its `.vlt`
  files, plus `.ts`/`.tsx` (not `.d.ts`) under a package's `src/` and `tests/`; skips
  `target/`, `node_modules/`, hidden dirs). `--check` writes nothing, lists unformatted files,
  exit 1 if any. Unparsable files → exit 1.
- Imports: `velt:x` → `<std root>/x.vlt` or `x/index.vlt`; `./x`, `../x` → relative `x.vlt`,
  `x.ts` or `x.tsx`, else folder module `x/index.vlt`, `x/index.ts` or `x/index.tsx` (two
  existing files among one of these triples: an error at the specifier naming them); `./x.vlt`,
  `./x.ts`, `./x.tsx` → exactly that file; `./x.js` → `x.ts` or `x.tsx`, `./x.jsx` → `x.tsx`
  (TypeScript's resolution). Module paths drop the extension (`x.ts` → `x`). JSX in a `.ts`
  module is an error (jsx.md). Bare names → `paths` aliases of the importing package first
  (the target resolved like a relative path), then packages via `package.vlt` (`pkg` →
  `src/lib.vlt`, `.ts` or `.tsx`; `pkg/sub` → `src/sub.*` or `src/sub/index.*` with the same
  three extensions; a bare specifier with a source extension is an error). The names a
  specifier spells must match the disk in case on every OS (else an error naming the file on
  disk and the corrected specifier); a file module that hides a folder module whose `index` has
  another extension gets a warning. Root files may be `.ts` or `.tsx` too; a package without
  `entry` uses `src/main.vlt`, `.ts` or `.tsx` (`src/lib.*` for a library). `std/prelude/*.vlt` is loaded implicitly before everything else.
  Global modules in `std/prelude/global/*.vlt` (`fetch`, `Response`, `URL`, …) join the prelude
  only when a non-std module mentions one of their names as a whole word, unless that module
  binds the name itself at the top level.
- Environment: `VELT_STD` (std root), `VELT_HOME` (default `~/.velt`), `VELT_REGISTRY`
  (default `$VELT_HOME/registry`), `VELT_RT_LIB` (runtime lib), `VELT_RT_LINK` (`static`: no shared runtime in debug builds), `VELT_LINKER` (`bundled`, `system`, or a linker program), `VELT_CLANG` (clang for the LLVM backend), `VELT_LLVM_OPT` (clang `-O` level of release builds, default 3), `VELT_CODEGEN_UNITS` (codegen units of LLVM release builds; default from the program's size).
  Set by `velt dev` for the program (not for users): `VELT_DEV_SOCKET` (a Unix socket path, or a
  named pipe `\\.\pipe\velt-dev-<pid>-<n>` on Windows).
- Lockfile: `version = 1` + `[[package]]` entries with `name`, `version`, `source`
  (`"registry"` | `"path+<rel>"`), `checksum`, `dependencies`.
