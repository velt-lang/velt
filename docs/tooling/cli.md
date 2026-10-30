# The `velt` command

One binary is the whole toolchain: compiler, runner, test runner, formatter, package manager,
language server and documentation generator. `velt <command> --help` (or `velt help <command>`)
prints every command's options and examples.

| Command | What it does |
|---|---|
| `velt new <name>` | create a package in a new directory from a template |
| `velt init` | turn the current directory into a package; `--editor vscode` only adds the VS Code files |
| `velt build` | compile a file or the current package |
| `velt run` | build and run a file or the current package |
| `velt check [--json]` | type-check a file or the current package without building it ([`velt check`](#velt-check)); `--ts-compat` lints code shared with TypeScript ([below](#code-shared-with-typescript---ts-compat)) |
| `velt dev` | run, then hot-swap or restart on every change ([`velt dev`](dev.md)) |
| `velt test` | run the tests ([Testing](../book/testing.md)) |
| `velt fmt` | format `.vlt` (and `.ts`, `.tsx`) files ([Formatter](fmt.md)) |
| `velt clean [--cache]` | remove the package's `target/` directory; `--cache`: the machine's [build cache](#build-cache) |
| `velt cache warm` | compile the standard library into the [build cache](#build-cache) ahead of a first build |
| `velt add`, `install`, `update`, `publish`, `native build` | packages ([Packages](packages.md)) |
| `velt manifest [--json]` | check the package's manifest, or print it as JSON for other tools ([`package.vlt`](manifest.md#other-tools)) |
| `velt search`, `yank`, `owner` | find and manage published packages ([Registries](packages.md#registries)) |
| `velt login`, `logout` | store or forget your token for a registry server ([Users, owners and yanking](packages.md#users-owners-and-yanking)) |
| `velt doc` | generate HTML API documentation, or JSON for other tools ([`velt doc`](#velt-doc)) |
| `velt lsp` | the language server ([Editors](editors.md)) |
| `velt playground` | write and run programs in the browser ([WebAssembly](webassembly.md#the-playground)) |
| `velt registry serve`, `registry user`, `registry owner` | serve a package registry over HTTP, and manage its users and owners ([Packages](packages.md#registries)) |
| `velt target list`, `add`, `remove` | the targets `velt build --target` builds for: install or remove target packs ([Cross-compiling](platforms.md#cross-compiling)) |
| `velt toolchain list`, `install`, `remove`, `prune`, `default`, `which`, `link` | the velt versions installed side by side, and which one runs ([`velt toolchain`](#velt-toolchain)) |
| `velt doctor` | check the installation and run a hello world |
| `velt completions <shell>` | print a completion script for bash, zsh, fish or PowerShell |

## Files and packages

Every build command works on a single file or on a package:

- **A file**: `velt run hello.vlt` builds `./target/velt/hello` (`hello.exe` on Windows),
  relative to the current directory, and runs it; a file inside a package is built into that
  package's `target/velt/`, wherever `velt` runs, so the members of a
  [workspace](packages.md#workspaces) keep their programs (and [build stamps](#build-cache))
  apart. The file, and the modules it imports, may also
  be `.ts` or `.tsx` files ([TypeScript files](../reference/modules.md#typescript-files-ts-and-tsx)). If the file is inside a package, the package's
  dependencies are installed first.
- **A package**: without a file argument, `velt` searches upward from the current directory for
  `package.vlt` and builds the package's entry (default `src/main.vlt`) to
  `<package>/target/velt/<name>`. A library-only package can't be run.

### Workspaces

In a [workspace](packages.md#workspaces), `velt check`, `test`, `build`, `fmt` and `doc` take
`--workspace`, `-p <glob>` (repeatable; a member's name, or a scoped name without its scope) and
`--affected [<base>]` to run on several members, and
run on every member at the workspace's root outside its packages; `--fail-fast` starts no more
members after a failure. `velt run -p <member>` and `velt dev -p <member>` run one member's
program, with the program's arguments after `--` (`velt run -p server -- --port 8080`). A file
argument doesn't go with a selection.

## `velt build` and `velt run`

```
velt build [<file.vlt>] [-o <out>] [--release] [-g] [--target <triple>] [--cpu <name>]
           [--backend cranelift|llvm] [--emit vir|llvm|obj|exe] [--locked] [--json] [-v]
           [--timings] [--report numbers]
velt run   [<file.vlt>] [--release] [-g] [--target <triple>] [--cpu <name>]
           [--backend cranelift|llvm] [--locked] [-v] [--timings] [--report numbers]
           [-- <program args>...]
```

- **Debug builds** (the default) use Cranelift: fast to compile, with line tables for debuggers
  on Linux and macOS (function symbols on Windows).
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
- Nothing is compiled twice ([below](#build-cache)): a build whose inputs did not change since
  the last build of the same output only links (or does nothing), and code compiled once on a
  machine is taken from the build cache by every later build, in any project.
- `--target wasm32-wasip1` and `--target wasm32-unknown-unknown` build WebAssembly
  ([WebAssembly](webassembly.md)); `--target x86_64-apple-darwin` cross-builds on Apple
  silicon.
- `--cpu <name>` chooses the CPU the program is built for: `baseline`, `x86-64-v2` (the default
  on x86_64: every x86_64 CPU since about 2009), `x86-64-v3`, `x86-64-v4`, `native` (this
  machine's CPU) or a CPU name clang knows for the target, such as `znver4` or `neoverse-n1`
  (an unknown one is an error that names the closest). Without it,
  `$VELT_CPU` and then the package's `build.cpu` decide; `velt run` and `velt test` without
  `--release` build for this machine's CPU. A program built for x86-64-v2 or above checks the
  CPU when it starts and names what is missing instead of crashing. Levels, defaults per
  target and the check: [Platforms, CPU](platforms.md#cpu).
- `--emit vir` / `--emit llvm` print the intermediate representation and stop; `--emit obj`
  writes only the object file.
- `-v` prints per-stage timings; `--timings` adds each optimizer pass and code generation step.
- `build --json` prints one JSON document on stdout instead of diagnostics on stderr, for
  editors (the VS Code extension's F5 runs it): `{"executable", "debugInfo", "lldbScript",
  "diagnostics", "errors", "warnings"}`. `executable` is the absolute path of the program
  (`null` if the build failed or made none, as with `--emit obj`); `debugInfo` says whether it
  has line information for debuggers (`false` for the default build on Windows, which has
  none yet); `lldbScript` is the toolchain's LLDB script for Velt values
  (`share/velt/lldb/velt_lldb.py`, `null` if missing); the diagnostics are those of
  [`velt check --json`](#velt-check). It exits like `velt build`.
- `--report numbers` lists, on stderr, the `number` variables assigned inside loops that the
  optimizer keeps as doubles, with the first reason it found for not storing each one as an
  integer:
  - `it may hold a fraction`
  - `it may be NaN`
  - ``it may reach ±2^53 (values in [lo, hi]): no loop bound or `| 0` limits it``
  - `it is set from a division, a call or memory`
  - `it may be -0, which line N can tell from 0` (or `which a use can tell from 0`)
  - `it is set from a value that is not a 32-bit integer`
  - `it is a parameter, a call result or stored in memory`

  A variable that is only converted from an integer and used as a double is not listed: as an
  integer it would only add conversions. A `number` that is whole and bounded is stored as an
  integer, with exactly the double's results:

  ```
  numbers: 1 `number` variable in loops stays doubles
    grid.vlt:7:7: `s`: it may hold a fraction
  ```
- `run` exits with the program's exit code. As with `node file.js a b`, the arguments after the
  file go to the program (`velt run app.vlt --port 8080`), so options for Velt come before
  the file. Without a file (the package's entry) program arguments follow `--`
  (`velt run -- --port 8080`); a `--` right after a file is accepted too. Compile errors exit
  with 1 and run nothing.

### Build cache

A build never compiles the same thing twice:

- **Nothing changed.** `velt build`, `velt run` and `velt test` record what a build read in a
  stamp beside the program (`<program>.build-stamp`): every source file (std, packages, `.d.ts`
  files) by content, every path imports were looked up at and every directory listed for them,
  `package.vlt`, `package.json`, `tsconfig.json` and `velt.lock.json`, the build's options and
  the compiler. When none of that changed, the next build parses and compiles nothing; running
  the program again costs only its start. Saving a file without changing it changes nothing
  either; a file changed while a build read it counts as changed. A new file that an import
  would now find (a `.ts` beside a `.vlt`, a package in a `node_modules` nearer the importer)
  counts as a change. A
  build that printed warnings gets no stamp, so the warnings show every time.
  `VELT_BUILD_STAMP=off` compiles every time.
- **Something changed.** The code each function compiles to is kept in a cache that every build
  on the machine shares, whatever the project: `$VELT_HOME/build-cache` (`~/.velt/build-cache`),
  or `VELT_BUILD_CACHE`. A debug build compiles only the functions whose code differs from any
  compiled before, on every core; std's functions, a package's, and generic instances such as
  `Map<string, number>` compile once per machine. A release build takes each codegen unit's
  object from the cache when clang compiled the same code with the same flags before.
- Entries are found by their contents, the target, the CPU and the version of what compiled them,
  so they are never out of date, and projects pinned to different Velt versions keep their
  entries side by side.
- The cache stays under `VELT_BUILD_CACHE_SIZE` (default `2G`) of disk space: about once an
  hour, a build that added code removes the code used least recently until the cache is under
  three quarters of that. Small entries (most compiled functions) are packed into a few files
  instead of a file each, so they take the space of their contents. `velt clean --cache` removes it all
  (only what the cache made: a `VELT_BUILD_CACHE` directory that holds other files keeps them);
  `VELT_BUILD_CACHE=off` builds without it.
- Entries are used as they are, so a `VELT_BUILD_CACHE` directory must be writable only by users
  you trust: whoever can write to it can put code into your builds.
- **Warm from the start.** Installing a Velt version (the install scripts, `velt toolchain
  install`, or the launcher installing the version a package pins) starts `velt cache warm` in
  the background at low priority: it compiles a set of programs shipped with the toolchain
  (`share/velt/warm/`) that reach most of std, for this machine's CPU (what `velt run` and
  `velt test` build for) and the default one (`velt build`). A project's first build then
  compiles little more than its own code. It runs once per Velt version; `velt cache warm` runs
  it in the foreground, `--background` detached. The installers skip it in CI (`CI` set), where
  the cache goes away with the runner, and when `VELT_CACHE_WARM=off`.
- **Std compiled once (experimental).** With `VELT_STD_OBJECT=1`, a release build compiles each
  larger std function it uses on its own, stores it in the build cache, and links it; the next
  program that compiles the same std function links it from the cache instead of compiling it
  again. `VELT_STD_OBJECT_STATS=1` shows what it found, compiled and left in the program.
- `velt build -v` shows how much came from the cache (`codegen ... (637 of 637 functions from
  the build cache)`) and when a build was up to date.

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
  skipping `target/`, `node_modules/`, hidden and symlinked directories, and nested packages:
  a directory with its own `package.vlt` is that package's, so check it there; `.d.ts` files are
  not modules), including `src/lib.vlt` next to `src/main.vlt`, modules nothing imports, and test
  files. The entry (`package.entry`, default `src/main.vlt`) must define a valid `main`; every
  other module is checked as a library module. A library package (no configured entry and no
  `src/main.vlt`) checks `src/lib.vlt` and the rest the same way. All modules are checked
  together, so a module several of them import is checked, and its errors reported, once. Two
  files in one directory whose names differ only in the extension (`src/dup.vlt` and
  `src/dup.ts`) are an error: an import can't tell them apart. Other
  directories (`examples/`, `bench/`, scripts next to `package.vlt`) usually hold programs of
  their own: check them with `velt check <file>`.
- `velt check <file>` checks that file and the files it imports, and nothing else.
- A syntax error stops a build, but not `velt check`: it reports every file's syntax errors, and
  then the type errors of the files without any, as `tsc` does. A file importing one with a
  syntax error is still checked; a name it imports that the broken file mentions but doesn't
  declare (the error may have cut the declaration short) reports nothing, and neither do its
  uses.
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
- Some rules need types: they ask the checker about what a name refers to and what type a
  value has (`=== null` on a `Map.get`, `${xs}` of an array, `sort()` of numbers), so the
  files are checked once more for them. That about doubles the time of the check and grows
  linearly with the code: 21,000 lines (3,000 small functions) take 155 ms to check and 304 ms
  with `--ts-compat` (a release build on an Apple M4).
- A finding that depends on values the lint can't see is a warning (`${x}` where `x` may be
  `undefined` in JavaScript, string lengths and positions, `xs.length - 1`); the others are
  errors.
- It exits with 1 when there is an error, from the check or from the lint; warnings alone
  don't fail it.
- The rules and the subset are listed in
  [the TSX design](../internals/design/tsx.md#the-common-subset). `velt build` never runs the
  lint.

## `velt new` and `velt init`

```
velt new <name> [--template app|cli|api|websocket|lib] [--lib]
velt init [--template <t>] [--name <name>] [--force]
velt init --editor vscode [--dir <dir>]
```

| Template | What you get |
|---|---|
| `app` (default) | hello world split into a module, with a test |
| `cli` | a command-line tool: `velt:cli` argument parsing, subcommands, `--help`, exit codes, tests |
| `api` | a JSON HTTP API: routes, validation, typed errors as status codes, tests with `fetch` against a live server |
| `websocket` | a WebSocket chat server and terminal client, with an end-to-end test |
| `lib` | a library: exports with `/** */` doc comments for `velt doc`, tests, ready for `velt publish` |

Every template builds, passes `velt test` and is formatted. `velt init` writes the same files
into the current directory; it never overwrites files without `--force`, always keeps an
existing `README.md`, and names the package after the directory unless `--name` is given.

Packages also get a `.gitattributes` line, `*.vlt linguist-language=TypeScript`, so GitHub
highlights `.vlt` files: GitHub has no Velt language yet, and Velt is TypeScript syntax plus a
few keywords and types. Like `.gitignore`, it is added to an existing file.

Packages also get `.vscode/launch.json`, so F5 in VS Code builds and debugs the program, and
`.vscode/extensions.json`, which recommends the Velt and CodeLLDB extensions
([Debugging](debugging.md#vs-code)); existing ones are kept. `velt init --editor vscode` adds
only those two files, to the root of the current package (or to the current directory outside
a package, or to `--dir`), and never overwrites one.

## `velt doc`

```
velt doc [<file|dir>...] [--std] [--format html|json] [-o <dir|file>]
```

Generates HTML documentation for exported items: their signatures and the doc comment right
above each declaration, a JSDoc `/** … */` comment or a block of `///` lines (a comment block at
the top of a file, followed by a blank line, documents the module). Plain `//` comments are not
documentation; on the lines between a doc comment and its declaration
(`// eslint-disable-next-line`) they are skipped. Without paths, it documents the package's `src/` (`.vlt`, `.ts` and `.tsx` files) into
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
- **Tags** in doc comments are rendered as sections: `@param name - text` as a parameter list
  (a `{type}` is ignored, as the signature shows the type), `@returns` (`@return`), `@throws`,
  `@example` (code, shown in a code block), `@deprecated [text]` (the item is also marked
  deprecated) and `@see`. Inline `{@link name}` shows `name` as code; `{@link name text}` shows
  the text; a URL target becomes a link. Other tags stay in the text as written.

### JSON output

`velt doc --format json` writes the same documentation as one JSON document, for tools that
render API pages in their own design (the docs website builds its API reference, its search
index and the standard library catalog from it). `-o` names a `.json` file, a directory (which
gets `api.json`; the default is the same directory as the HTML) or `-` for stdout:

```
velt doc --std --format json -o std-api.json
```

The document is `{ "schemaVersion": 1, "title": …, "modules": [ … ] }`; `schemaVersion` changes
when a field changes meaning or goes away (new fields may be added without it). Every field is
always present (`null` or `[]` when empty). A module's record:

| Field | |
|---|---|
| `module` | its name in the docs (`std/fs`, `mypkg/util`); targets refer to modules by it |
| `specifier` | how it is imported (`velt:fs`, `mypkg/util`) |
| `file` | its source file (`std/fs.vlt`, `src/util.vlt`) |
| `doc`, `summary` | the module doc (Markdown) and its first sentence |
| `links` | the `{@link}`s in the module doc (as for items, below) |
| `exports` | its exported items, in source order, then the re-exported ones |

An item (an export, or a member in `members`, which has the same shape):

| Field | |
|---|---|
| `id` | a key unique in the document: `<specifier>#<name>` (`velt:http#serve`), `#<Type>.<member>` for a member (`velt:http#Server.close`); a setter is `<member>:set`, a static member whose name an instance member has too `static:<member>`, an `extend` block `extend:<type>` (`~2` for a second one on the same type) |
| `name` | its exported name |
| `kind` | `function`, `class`, `struct`, `interface`, `enum`, `type`, `const`, `extend` (an `extend` block, named after its type), `re-export` (from a module not documented alongside); members: `field`, `method`, `getter`, `setter`, `constructor`, `member` (of an enum) |
| `modifiers` | `static`, `async`, `readonly`, `abstract`, `protected`, `public`, `mut` as written, and `deprecated` |
| `signature` | the canonical signature (one line per overload) |
| `spans` | the type names in `signature` (of members too) that link to their documentation: `{start, end, role, target}`, offsets in UTF-16 code units (JavaScript string indices); `role` is where the name is: `param`, `return`, `throws`, `extends`, `implements`, `typeArg` (a bound or default of the item's own type parameters), `field` (a field's type) or `type` (a constant's or type alias's type, the type an `extend` block extends); a type argument has the role of the type it belongs to (`Promise<Point>` as a return type: `return`) |
| `doc`, `summary` | the doc comment's text without its tags (Markdown; `{@link}` already rendered as in HTML) and its first sentence |
| `tags` | `params` (`[{name, doc}]`), `returns`, `throws` (`[…]`), `examples` (`[…]`, verbatim), `deprecated` (its text, `""` without one), `see` (`[…]`) |
| `links` | the `{@link}`s in the doc comment and its tags: `{text, ref, target}`, `text` as the Markdown shows it, `ref` as written, `target` `null` when nothing documented has that name |
| `source` | `{file, line}` of the declaration (a re-exported item's is where it is declared); `null` for a `re-export` |
| `reexportedFrom` | `{module, name}` where a re-exported item is declared, else `null` |
| `members` | the public fields, accessors, constructor and methods (static ones too) of a class, struct or interface; an enum's members; an `extend` block's methods |

A `target` is `{id, module, kind, name, member}`: the item `name` that `module` declares, with
that `kind` (and its member `member`, or `null`); `id` is that item's (or member's) `id`, so
cards and backlinks can key on it. For example, a method of a class in a package:

```json
{
  "id": "pkg/lib#Point.moved",
  "name": "moved",
  "kind": "method",
  "modifiers": [],
  "signature": "moved(dx: f64, dy: f64): Point",
  "spans": [
    {"start": 25, "end": 30, "role": "return", "target": {"id": "pkg/lib#Point", "module": "pkg/lib", "kind": "class", "name": "Point", "member": null}}
  ],
  "doc": "The point moved by `dx`, `dy`.",
  "summary": "The point moved by `dx`, `dy`.",
  "tags": {
    "params": [
      {"name": "dx", "doc": "how far along x"},
      {"name": "dy", "doc": "how far along y; see `Point.length`"}
    ],
    "returns": "a new point",
    "throws": [],
    "examples": [],
    "deprecated": null,
    "see": []
  },
  "links": [
    {"text": "`Point.length`", "ref": "Point.length", "target": {"id": "pkg/lib#Point.length", "module": "pkg/lib", "kind": "class", "name": "Point", "member": "length"}}
  ],
  "source": {"file": "src/lib.vlt", "line": 64},
  "reexportedFrom": null,
  "members": []
}
```

## `velt toolchain`

Several velt versions can be installed side by side, each in `<root>/toolchains/<version>/`
(`<root>` is `~/.velt`, or `%LOCALAPPDATA%\velt` on Windows). The `velt` on your `PATH` is a
small launcher, `<root>/bin/velt`, that runs one of them with the same arguments. The
installers set it up ([Installing a release](platforms.md#installing-a-release)).

1. `velt +<toolchain> <command>` or `$VELT_TOOLCHAIN`, for one command: a version (`+0.2.0`),
   a requirement read like a pin (`+0.2`: the newest 0.2.x), or a linked name (`+dev`);
2. else the [`velt` field](manifest.md#velt) of the nearest `package.vlt`: the version the
   package's [`velt.lock.json`](manifest.md#veltlockjson) records when the field accepts it
   (installed first if it is missing), so every machine builds a commit with the same velt;
   otherwise the newest installed version the field accepts. `velt update` takes the newest
   *published* one the field accepts, installing it first, or the newest installed one when
   that is newer (without the network, the newest installed one),
   which then records itself in the lock;
3. else the default (`velt toolchain default`), which is also what loose `.vlt` files outside a
   package use.

A version the package pins that isn't installed is downloaded from the release on first use and
installed, with a message on stderr. A new patch version gets the target packs
([`velt target add`](platforms.md#cross-compiling)) the newest older patch of its minor had, so
`velt build --target …` keeps working after an update. The release's `SHA256SUMS` and the list of releases are
signed with the velt release key, which is built into velt, so a mirror or a replaced file can't
substitute a toolchain. A requirement such as `"0.1"` picks the newest release it accepts that
hasn't been yanked (withdrawn for a serious bug). Set
`VELT_TOOLCHAIN_AUTO_INSTALL=0` to turn that off: the command then fails and names the
`velt toolchain install` command to run.

```
$ velt toolchain which
0.1.4 (velt: "0.1" in /home/me/app/package.vlt:5)
/home/me/.velt/toolchains/0.1.4
$ velt toolchain list
*  0.2.0
 > 0.1.4
```

| Command | What it does |
|---|---|
| `list [--available]` | installed versions, newest first, and links; `*` marks the default and `>` the one this directory selects. `--available` lists the published versions, and marks the yanked ones, which a requirement such as `0.1` no longer selects |
| `install <version> [--default]` | install `0.1.3`, or the newest published version a requirement such as `0.1` accepts; the first one installed becomes the default |
| `install --from <file> [--unverified] [--default]` | install a toolchain archive downloaded before (`velt-<version>-<triple>.tar.gz` for this machine), without the network: checked against its release's `SHA256SUMS` and `SHA256SUMS.sig` beside it, or, with `--unverified`, not checked. Without the network it checks only the revocations this root already stored: a key revoked after the root last read the index of releases is still trusted. Installing a version that is already installed keeps it, and says so |
| `remove <toolchain> [--force]` | remove a version or a link. Debug executables link the runtime of the toolchain that built them, so the ones a removed version built stop running until rebuilt. The default needs `--force`. A version a running program has open (Windows) is reported in use and left whole |
| `self-update` | replace the launcher (`~/.velt/bin/velt`) with the one of the newest release, installing that release first if needed; the launcher says when a newer one is out. The new launcher is run before it replaces the old one, so one that can't run on this machine is refused and `velt` keeps working. Run it as `velt`, not a copy of the launcher under another name |
| `prune [--dry-run]` | remove every installed version but the default, the one this directory runs, those a link points into, and, for each `major.minor`, the one a pin like `"0.1"` runs (its newest release, or its newest pre-release when it has no release yet), saying how much space each frees; `--dry-run` only lists them. A version a lockfile still names is installed again when it is next used |
| `default [<toolchain>]` | show or set the default |
| `which` | the toolchain this directory runs, why, and its directory |
| `--version` | the launcher's own version, `velt-launcher <version>` (`velt --version` is the selected toolchain's) |
| `link <name> <prefix>`, `unlink <name>` | use a toolchain prefix built elsewhere (a checkout's; it holds `bin/velt` and `std/`) as `<name>`: `velt +dev test` |

The launcher runs `velt toolchain` itself, so it works the same whichever version a package
pins. A toolchain's own `bin/velt`, started directly, says so instead.

When the release that is about to run has a problem (it was yanked, or the index of releases
carries an advisory for it, such as a security one), the launcher says so on stderr, at most
once a day per version, with the newest release of the same minor that doesn't have it and how
to move to it (`velt update` in a package pinned to the version, or `velt toolchain install
<version>`). It reads the last index it fetched, so it needs no network. Text from the index is
shown with control characters replaced and cut at 200 characters.

## `velt doctor`

Checks the runtime library, the standard library, the linker (the bundled lld with the
target's link kit, or the system linker and why the bundled one is not used), the installed
target packs (`velt target`), the WebAssembly
linker (the bundled lld, or the Rust toolchain's `rust-lld` when Rust is installed), clang, the
debugger scripts (`share/velt/lldb/velt_lldb.py`, optional), that `VELT_HOME` is writable, and
which toolchain version runs and why (`velt toolchain`), then compiles and runs a hello world
(debug, plus release through LLVM when clang is found). Problems are marked `✗` (required) or
`!` (optional) with a `fix:` hint. It exits with 0 when every required check passes.

```
$ velt doctor
✓ velt             velt 0.1.1 (4efaa8d x86_64-pc-windows-msvc)
✓ runtime lib      C:\Users\me\AppData\Local\velt\toolchains\0.1.1\lib\velt_rt.lib
✓ std              C:\Users\me\AppData\Local\velt\toolchains\0.1.1\std
✓ linker           bundled C:\Users\me\AppData\Local\velt\toolchains\0.1.1\lib\velt\lld.exe (kit …\lib\targets\x86_64-pc-windows-msvc)
✓ wasm linker      C:\Users\me\AppData\Local\velt\toolchains\0.1.1\lib\velt\lld.exe
✓ clang            C:\Program Files\LLVM\bin\clang.exe
✓ debugger scripts C:\Users\me\AppData\Local\velt\share\velt\lldb\velt_lldb.py
✓ velt home        registry C:\Users\me\.velt\registry, cache C:\Users\me\.velt\cache
✓ toolchain        0.1.1 (the default); launcher C:\Users\me\AppData\Local\velt\bin\velt.exe, default 0.1.1
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
| `VELT_SHARE` | directory of the debugger scripts (`<it>/lldb/velt_lldb.py`; default `<prefix>/share/velt`) |
| `VELT_HOME` | package manager home (default `~/.velt`: `cache/`, `registry/`) |
| `VELT_REGISTRY` | package registry: a directory (default `$VELT_HOME/registry`) or an `http(s)://` URL |
| `VELT_REGISTRY_TOKEN` | a registry token sent to every registry server, overriding the tokens `velt login` stored (for CI) |
| `VELT_CA_FILE` | PEM file of extra CA certificates to trust for `https://` registries |
| `VELT_CLANG` | clang for the LLVM backend |
| `VELT_LLVM_OPT` | optimization level for release builds (clang, or `opt` for WebAssembly): `3` (default), `2`, `1`, `s` or `z` |
| `VELT_CPU` | the CPU builds are for when `--cpu` does not say ([CPU](platforms.md#cpu)); a CPU of another architecture does not apply |
| `VELT_CPU_DISABLE` | read by programs: features (`popcnt,sse4.2`) the start-up CPU check treats as missing, to test the check ([CPU](platforms.md#cpu)) |
| `VELT_CODEGEN_UNITS` | how many codegen units (parallel clang processes) an LLVM build uses, at most the core count; `1` turns splitting off; default: from the program's size (one unit below about 32 000 VIR statements) |
| `VELT_BUILD_CACHE` | the [build cache](#build-cache)'s directory (default `$VELT_HOME/build-cache`); `off` turns it off |
| `VELT_BUILD_CACHE_SIZE` | how large the build cache may grow (`500M`, `10G`; default `2G`) before the code used least recently is removed |
| `VELT_BUILD_STAMP` | `off`: compile even when no input changed since the last build of the same output |
| `VELT_RT_LIB` | runtime library (default: next to `velt`, or `<prefix>/lib` when installed) |
| `VELT_RT_LINK` | `static`: debug builds link the static runtime instead of the shared one ([Platforms](platforms.md)) |
| `VELT_NATIVE_FROM_SOURCE` | `1`: build a package's native library from source when no prebuilt one exists ([Packages](packages.md)) |
| `VELT_DEV_POLL`, `VELT_DEV_DEBUG_INFO`, `VELT_DEV_STOP_GRACE_MS` | `velt dev`: check files by polling, turn off JIT debug info (`0`), how long a stopped program may finish in-flight work in milliseconds (default 1500) ([`velt dev`](dev.md)) |
| `VELT_INSTALL_BASE_URL` | the repository whose releases the launcher, `velt toolchain install`, `velt target add` and the installers download from (default `https://github.com/velt-lang/velt`) |
| `VELT_TOOLCHAIN` | the toolchain the launcher runs for this command: a version or a linked name, overriding the package's `velt` field ([`velt toolchain`](#velt-toolchain)) |
| `VELT_TOOLCHAIN_AUTO_INSTALL` | `0`: the launcher fails instead of installing a missing version |
| `VELT_INSTALL_PUBLIC_KEY` | the Ed25519 keys (64 hex digits each, separated by commas) release signatures (`SHA256SUMS.sig`, `releases.json.sig`) are checked with, instead of the velt release keys: for the releases of another build |
| `VELT_LINKER` | `bundled`: link with the toolchain's lld and link kit, an error if it has none; `system`: the system linker (`link.exe`, `cc`); a path: that linker program, given the system linker's arguments; default: the bundled linker when the toolchain has it ([Platforms](platforms.md#the-bundled-linker)) |
| `VELT_LLVM_BIN` | directory with LLVM's `opt` and `llc`, for WebAssembly |
| `VELT_WASI_SYSROOT` | wasi-libc directory, for `wasm32-wasip1` |
| `VELT_WASM_RUNNER` | program that runs `wasm32-wasip1` modules for `velt run` (default: wasmtime) |
| `VELT_THREADS` | number of runtime worker threads (default: one per core) |
| `VELT_RESCUE` | `0`: the runtime never serves a worker that makes no progress (its timers, sockets and waiting tasks) from another one, and starts no spare workers. Together with `VELT_THREADS=1`, a program's work then no longer depends on how its threads are scheduled; with more workers, tasks still move to sleeping ones. For counting instructions only ([task placement](../std/task.md#task-placement)) |
| `MIMALLOC_*` | the allocator's options ([mimalloc](https://microsoft.github.io/mimalloc/environment.html)); Velt programs commit memory as they use it (`MIMALLOC_ARENA_EAGER_COMMIT=0`), which keeps a multi-threaded program's resident memory near what its data needs; `MIMALLOC_ARENA_EAGER_COMMIT=1` commits arenas up front again (on Linux with transparent huge pages, each worker then makes 2 MiB resident at once) |
| `VELT_SCHEDULER` | Unset or `velt` (the default): the runtime's own scheduler. Work stays on the worker that made it, and another worker is taken on when a worker's input (tasks woken by sockets, timers or other threads) has waited 200 µs or more on average over 10 ms, or the worker was awake for 95 % of those 10 ms with input waiting; a task woken by another task runs next on the waker's worker; a worker that runs out of work runs input waiting on another awake one, and one that makes no progress for 2 ms (a long computation, or the OS took it off its core) has its sockets, timers and half of its queue served by another. TCP, the HTTP server, WebSockets, Postgres over TCP and timers run on it natively, the rest (files, processes, Redis, `fetch`, UDP, DNS) on a small tokio runtime beside it. `stealing`: tokio's work-stealing scheduler (idle workers take tasks from busy ones). `per-core` (#211): one tokio event loop per worker; a task never moves after its first poll, so a CPU-bound task delays everything on its worker. |
| `MACOSX_DEPLOYMENT_TARGET` | oldest macOS a program runs on (default and minimum: 11.0 on arm64, 10.12 on x86_64) |
| `NO_COLOR` | disable colored output |
| `CLICOLOR_FORCE` | force colored output |
| `COLUMNS` | terminal width for `velt search` results (default: asked from the terminal) |
