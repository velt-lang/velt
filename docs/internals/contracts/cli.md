# `velt` CLI — CONTRACT

```
velt build [<file.vlt>] [-o <out>] [--release] [-g] [--backend cranelift|llvm] [--target <triple>] [--cpu <name>] [--emit vir|llvm|obj|exe] [--locked] [--json] [-v] [--timings]
velt run   [<file.vlt>] [--release] [-g] [--target <triple>] [--cpu <name>] [--backend cranelift|llvm] [--locked] [-- <program args>...]
velt check [<file.vlt>] [--json] [--locked] [-v]
velt check --ts-compat [<file|dir>...] [--json] [--locked] [-v]
velt dev   [<file.vlt>] [--exe [--release] [-g] [--cpu <name>] [--backend <name>]] [--locked] [-v] [--timings] [-- <program args>...]
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
velt doc [<file|dir>...] [--std] [-o <dir>]     # HTML API docs
velt registry serve [--dir <d>] [--port <n>] [--host <addr>]   # package registry server (default 127.0.0.1:8091)
velt doctor                            # checks toolchain setup + smoke test
velt toolchain list [--available] | install <v> [--default] | remove <t> [--force] | self-update | prune [--dry-run] | default [<t>] | which | link <name> <prefix> | unlink <name> | --version   # the launcher's (#948)
velt --version                         # velt <ver> (<git hash> <host triple>)
```
- **Single file**: `build <file>` writes `./target/velt/<stem>` (`.exe` on Windows) relative to the
  current dir, object file next to it. If the file lives inside a package, its deps are installed first.
- **Package mode** (no file argument): finds `package.vlt` upward from the cwd, builds its entry, output
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
  `test --jobs <n>` / `-j <n>` (additive): how many test files run at once (default: one per
  core); the output is the same for any `n`, in file order.
- `new` / `init` (additive): every template also writes `.vscode/launch.json` (one
  `{"type": "velt", "request": "launch", "name": "Debug"}` configuration) and
  `.vscode/extensions.json`, both kept if present. `init --editor vscode` writes only those, in
  `--dir` (an existing directory) or else at the root of the package around the cwd (or in the
  cwd outside one), never overwriting, and takes no other option.
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
  (`<exe>.link-stamp` beside it).
- Build stamp and build cache (additive, #1612). `build`, `run` and `test` with `--emit exe`
  (the default) and no `--report` skip parsing, checking and codegen when `<exe>.build-stamp`
  matches: the compiler (version, executable size and time), the options, a generated root's
  source, the `VELT_*` environment (but `VELT_BUILD_CACHE*`, `VELT_BUILD_STAMP`,
  `VELT_CACHE_STATS`, `VELT_TEST_PLAN`), clang for LLVM builds, every file the build read (the
  session's source map and the configuration files module resolution read) by content hash (the
  size and time it had when it was read first, not recorded when it was modified less than 2 s
  before it was read, and not trusted when not older than the stamp), what
  module resolution looked at (`vpm::probe`: every path probed is still a file, a directory or
  missing as it was, every directory listed has the same entries), the manifests and lockfile
  of the package graph, and by size and time the unit objects and the executable it wrote (so
  an output another build or tool rewrote is a change), extra link inputs, and the std cache's
  objects the link read (`std` lines; stamp format `velt-build-stamp 3`). The objects it names
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
  `warm/` and the marker (the root too when it is then empty); layout `v3/<kind>/`
  with kinds `cl` (Cranelift compiled functions, Cranelift's incremental-cache key with the
  Cranelift version), `stdfn` (the std cache below: objects of single std functions, file
  entries) and `llvm`
  (unit objects, keyed by clang's path, size, time and arguments, the host triple and the IR).
  Entries under 16 KiB are packed: 64 shards per kind by the key's first byte, each `<nn>.pack`
  (values) + `<nn>.idx` (56-byte records: key, offset, length, 8-byte SHA-256 prefix of the
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
- Std cache (additive, #1728 step 3; off by default). Release builds with `VELT_STD_OBJECT=1`
  take std functions out of the program: each std function that is not small (40 statements),
  not a `$dup` rename, reaches no static with relocations or written (named by it or by a small
  function it may inline) and makes no indirect call is keyed by SHA-256 over `velt std function
  1`, `build_cache::compiler_identity()` (version line, executable size and time), clang's path,
  size and time, the target, the CPU and the optimization level, then, for it and each small
  internal function it reaches through small ones (in that order), its symbol,
  `velt_vir::fingerprint::Canon::text` (`Mode::Link`) and its parameter attributes. Before optimization,
  each found key (kind `stdfn`, `Cache::path_of`), and each missing one when the missing
  functions come to at least 1,000 statements (else they stay in the program), makes the function
  an `Import` declaration; functions their objects may reference are made `Export`. After
  optimization the objects of the functions still declared, closed over what those objects call,
  are linked; a missing one is compiled then, alone (itself, the small functions it may inline
  with their bodies, declarations of what they call, the statics they name; optimized at
  `Speed`; LLVM, one unit; the entry declared, vir.rs invariant 5) and stored (`Cache::put_file`).
  If one fails, the build optimizes the program as lowered, without the cache. `VELT_STD_OBJECT_STATS=1` prints, on stderr, the std
  statements by reason (`served`, `small`, `$dup`, `statics with identity`, `callbacks`), each
  linked symbol (`velt: std cache: links <symbol> (<n> statements)`) and `velt: std cache: <n>
  found, <m> compiled, <k> no longer called`.
- The launcher (additive, #948; crates/velt_launcher, binary `velt-launcher`, installed as
  `<root>/bin/velt[.exe]`; `<root>` is the launcher's grandparent directory, else it exits 1).
  A first argument `+<toolchain>` is removed and selects that toolchain (reason `+<toolchain>`);
  a toolchain's own `velt` given one exits 1 saying it is the launcher's. For every command but
  `toolchain` it selects `+<toolchain>`, else `$VELT_TOOLCHAIN` (both: a full version is that
  version, a `[a-z]…` name a link, anything else a requirement read like a pin),
  else the `velt` requirement of the nearest `package.vlt` above the cwd
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
  `--timings` adds each optimizer pass and, with the LLVM backend, IR printing and clang.
- Debug info: debug builds always carry it; `-g` keeps it in a `--release` build (and links with
  debug settings: PDB on Windows, no `-s` strip on Linux). With the LLVM backend it is full line
  info (`.vlt` file:line in debuggers and symbolizers); with Cranelift, DWARF line tables in ELF,
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
  `clean --cache` removes the build cache's directory instead (same output; the cache turned off
  → `Clean nothing to remove (the build cache is off: $VELT_BUILD_CACHE)`).
- `cache warm [--background]` (additive, #1612): with the build cache on, builds each
  `*.vlt` of the warm-up directory (`$VELT_SHARE/warm`, else `share/velt/warm` beside the std
  root's directory: `<prefix>/share/velt/warm`, or the checkout's) with a child `velt build
  <file> -o <tmp>/warm[.exe]` (an executable; `VELT_BUILD_STAMP=off`, no `VELT_CPU`), once with `--cpu native` and once without, then
  writes the marker `<cache>/warm/<hex>` (SHA-256 of the version line and the executable's size
  and time) and prints `velt: warmed the build cache with <n> programs in <s>s`. With the marker
  present it prints that the cache is warm and does nothing; one run per marker at a time
  (`<marker>.lock`). A program that fails to build is reported (exit 1, no marker) after the
  others ran. `--background` starts `velt cache warm` detached (own process group, lowest
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
  Global modules in `std/prelude/global/*.vlt` (`fetch`, `Response`, `URL`, `JSON`, …) join the prelude
  only when a non-std module mentions one of their names as a whole word, unless that module
  binds the name itself at the top level.
- Environment: `VELT_STD` (std root), `VELT_HOME` (default `~/.velt`), `VELT_REGISTRY`
  (default `$VELT_HOME/registry`), `VELT_RT_LIB` (runtime lib), `VELT_RT_LINK` (`static`: no shared runtime in debug builds), `VELT_LINKER` (`bundled`, `system`, or a linker program), `VELT_CLANG` (clang for the LLVM backend), `VELT_LLVM_OPT` (clang `-O` level of release builds, default 3), `VELT_CODEGEN_UNITS` (codegen units of LLVM release builds; default from the program's size), `VELT_BUILD_CACHE`, `VELT_BUILD_CACHE_SIZE`, `VELT_BUILD_STAMP` (above), `VELT_EAGER_STD` (set: check every standard-library definition, not only those the program reaches; docs/internals/design/lazy-std.md).
  Set by `velt dev` for the program (not for users): `VELT_DEV_SOCKET` (a Unix socket path, or a
  named pipe `\\.\pipe\velt-dev-<pid>-<n>` on Windows).
- Lockfile: `version = 1` + `[[package]]` entries with `name`, `version`, `source`
  (`"registry"` | `"path+<rel>"`), `checksum`, `dependencies`.
