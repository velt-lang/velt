# Editors and the language server

`velt lsp` is a Language Server Protocol server on stdin/stdout. It works in any editor with an
LSP client; the repository ships a Visual Studio Code extension in `editors/vscode`.

## Features

- **Diagnostics** from the parser, the module loader and the type checker, as you type. Unsaved
  changes of other open files are taken into account.
- **Formatting** with [`velt fmt`](fmt.md).
- **Navigation**: document outline, go to definition (also through `ns.x` namespace imports and
  re-exports, landing on the original declaration), find references, rename, document highlight
  (reads and writes), workspace symbols (the workspace folders are indexed once and kept up to
  date from the editor's file change events, or by checking modification times when the editor
  does not send them).
- **Hover**: declaration signatures, including inferred `throws` types, and the inferred type of
  locals and expressions. Below the signature comes the declaration's doc comment (a `/** … */`
  block or `///` lines right above it; see [doc comments](../reference/lexical.md)), rendered
  as Markdown: the description, then `@param`, `@returns`, `@throws`, `@example`, `@deprecated`
  and `@see`. Hovering a parameter shows its `@param` text. This works for your own code,
  imports and the standard library (the prelude's methods and the `fetch` globals included):
  functions (generic ones too), classes and their members (constructors, methods, getters,
  static members, and `#private` members inside the class), interfaces, enum variants, and
  the fields of object types and of intersections (`A & B`).
- **Completion**: locals, module items, imports, prelude items, keywords, and members after `.`
  (also while the file doesn't parse). In JSX: tag names after `<` (the elements of the JSX
  runtime's `JSX.IntrinsicElements` and the components in scope) and attribute names inside an
  opening tag (the element's attributes or the component's props, minus those already written).
  After `</` the element still open there comes first. Tags are offered once the file contains
  JSX that parses, which is when its JSX runtime loads. The editor shows the doc comment of the
  selected item (JSX tags and attributes included), and items documented `@deprecated` are
  struck through.
- **Imports**, as in TypeScript editors (all of it also while the import doesn't parse yet):
  - inside the braces of `import { … } from "velt:fs"`, completion offers the module's exports
    with their signatures, minus the names already listed (`import type { … }`: types only);
  - inside the quotes after `from` (or `import("…")`), completion offers module specifiers: the
    standard library's modules (`velt:fs`, `velt:collections/set`, with a line about each),
    files and folders next to the file for `./` and `../`, and the package's dependencies.
    Typing `"` or `/` there opens the list. Files are named as imports name them: without the
    extension, unless two files share a name (`./dup.ts` next to `./dup.vlt`);
  - **auto-import**: typing a name that isn't imported yet offers the exports of std modules,
    of the dependencies and of the package's other files (unsaved changes of open files
    included) that start with what you typed, marked with the module (`readFile  velt:fs`).
    Accepting one adds it to the file's `import { … } from` that module (in order, if its names
    are sorted), or adds that import after the other imports. A file is named so that the
    import loads it: with its extension when another file shares its name, and a folder module
    as `./shapes/index` when a file `shapes.vlt` would win over the folder.
  - the exports offered inside the braces and by auto-import show their doc comments, and
    `@deprecated` ones are struck through.
- **JSX**: go to definition, hover, references and rename on tags, opening and closing
  (`<Card` and `</Card>` → `function Card`), and on attributes (the attribute's or prop's
  declaration and type).
- **Signature help** while typing call arguments, with the function's description, return value
  and exceptions from its doc comment, and each parameter's `@param` text.
- **Deprecation**: uses of a definition documented `@deprecated` get a hint with the reason, and
  editors show them struck through.
- **Inlay hints**: inferred types of `const` / `let` / `for...of` bindings and parameter names at
  call sites. On declarations, what inference decided ([memory model](../reference/memory.md#mutation-is-inferred),
  [errors](../reference/errors.md)): `throws E` after a function without a `throws` clause that
  can throw, `modifies this` after the signature of a method that modifies its receiver, and
  `modified` before each parameter whose contents the function modifies.
- **Semantic highlighting**: types, functions, methods, parameters, properties and enum members;
  `let` bindings carry a `mutable` modifier, and calls of functions and methods declared in
  Velt source that modify their receiver or an argument a `mutating` modifier (built-in methods
  such as `push` are not marked), which you can style.
- **Quick fixes** for compiler errors: remove `mut`, replace `undefined` with `null`, turn
  `"a" + xs` (a string and a value `+` does not convert, such as an array) into a template
  literal, replace `export default` with a named export, add `await`
  or `spawn(...)` to a floating promise, add `async` to a method whose promise must carry
  its errors, and import a name the file uses without importing it ("Import `readFile` from
  `velt:fs`", one fix per module that exports it). A fix that applies in several places is
  also offered as "Fix all in file", and **Fix all** (`source.fixAll`, e.g. on save) applies every preferred
  fix of the file.

- **Code shared with TypeScript**: in a file inside one of the package's
  [`tsCompat`](manifest.md#tscompat) folders, the findings of
  [`velt check --ts-compat`](cli.md#code-shared-with-typescript---ts-compat) appear as you type,
  with the rule as the diagnostic's code and `velt ts-compat` as its source (see
  [below](#code-shared-with-typescript)).
- **Package manifests**: `package.vlt` is read as data, the way `velt` reads it, not checked as a
  program. Its diagnostics are exactly `velt`'s; completion offers the fields valid at the cursor
  and fixed values (native targets, `true`/`false`); hover explains each field. Saving a changed
  `package.vlt` reinstalls the package's dependencies for the other open files
  ([`package.vlt`](manifest.md)).
- **Live registry data** in `package.vlt`, from the package's registry: completion of versions
  (typing `"` after a dependency's name) and of package names in `dependencies`; hover on a
  dependency shows its newest and locked versions; a requirement no published version matches,
  or a package the registry doesn't have, is an error, a requirement that leaves out a newer
  version gets an informational note, and a quick fix moves it to `^<newest>`. The data is fetched in the
  background; an offline registry just adds nothing. A manifest's `registry` is asked only when
  it is `https://` or on this machine (`$VELT_REGISTRY` always is), so opening a checkout never
  makes the editor contact a plain-HTTP host it names.

The server answers even when the program has errors, and a failing request never takes the
server down.

`.ts` and `.tsx` modules ([TypeScript files](../reference/modules.md#typescript-files-ts-and-tsx))
are part of the programs the server analyzes, and it asks the editor to report changes to them.
The workspace symbol index takes `.vlt` files anywhere in the workspace folders, but `.ts` and
`.tsx` files only in a package's `src/` and `tests/` (and open documents), so the TypeScript
frontend of a monorepo is not indexed as Velt. Whether the editor sends it `.ts` and `.tsx` documents to
analyze is up to the client: the VS Code extension leaves them to VS Code's TypeScript support.

## Code shared with TypeScript

A file in a folder that `package.vlt`'s `tsCompat` lists gets the TypeScript-compatibility
lint's findings next to the compiler's diagnostics: errors where `tsc` would reject the code or
JavaScript would run it differently, warnings where it may. Each finding's code is its rule
(`velt-number-type`) and its source is `velt ts-compat`; the message ends with what TypeScript
does and what to write. A finding with a mechanical replacement (`f64` → `number`, `bool` →
`boolean`, a dropped suffix) offers it as a preferred quick fix, so **Fix all** applies it too.

- The findings follow your edits, also unsaved edits to `package.vlt`'s `tsCompat`; closing
  `package.vlt` without saving goes back to the file on disk. Changes made outside the editor
  (`package.vlt` saved by another program, folders created or deleted) apply when the editor
  reports them (VS Code does). A folder `tsCompat` lists that doesn't exist is a warning in
  `package.vlt`.
- The files in the folders are the ones `velt check --ts-compat` finds: not under
  `node_modules/`, `target/`, hidden or symlinked directories, or in a nested package.
- Like the command, the lint skips a file with errors of its own: fix those first.
- An import of a file outside the folders is the finding `outside-import`.
- Files outside the folders never get findings, and nothing changes for them.
- The VS Code extension sends only `.vlt` files to the server; `.ts` and `.tsx` files in the
  folders get findings in editors that send them, and from `velt check --ts-compat`.

## Visual Studio Code

The extension adds syntax highlighting, starts `velt lsp`, and debugs Velt programs with F5
([Debugging](debugging.md#vs-code)). Above `main` the server shows **▶ Run | Debug** to a client
that asks for these lenses (`"initializationOptions": {"runLenses": true}`, as the VS Code
extension does): they run the extension's `velt.runFile` and `velt.debugFile` commands, so
other editors don't get them.

1. Build or install `velt` and put it on `PATH`, or set `velt.serverPath` to the executable.
2. Build and install the extension (Node.js 18 or newer):

   ```sh
   cd editors/vscode
   npm install
   npx tsc -p .
   npx @vscode/vsce package --allow-missing-repository --skip-license   # → velt-0.1.0.vsix
   code --install-extension velt-0.1.0.vsix
   ```

| Setting | Default | Meaning |
|---|---|---|
| `velt.serverPath` | `velt` | the `velt` executable (`~` and `${workspaceFolder}` are expanded); changing it restarts the server |
| `velt.trace.server` | `off` | `messages` / `verbose` log the LSP traffic to the output channel |
| `velt.debug.engine` | `auto` | the debugger extension F5 starts: `codelldb`, `lldb-dap`, `cpptools`, or `auto` (the first installed, in that order) |

With the default `velt.serverPath`, the `velt` on `PATH` is the launcher, which starts the
language server of the toolchain the first workspace folder's package pins
([`velt toolchain`](cli.md#velt-toolchain)). A window holding packages pinned to different
versions uses that one for all of them; open each in its own window to get its own.

The command **Velt: Restart Language Server** restarts it, for example after rebuilding `velt`.
**Velt: Run File** and **Velt: Debug File** run or debug the open file (also as buttons in the
editor title), and **Velt: Generate launch.json** writes `.vscode/launch.json` like
`velt init --editor vscode`. For debugging, see [Debugging](debugging.md#vs-code).

**Not yet available**: a published Marketplace extension, and packaged support for other
editors. Any editor that can start `velt lsp` as a language server works today.
