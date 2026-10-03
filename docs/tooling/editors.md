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
  locals and expressions.
- **Completion**: locals, module items, imports, prelude items, keywords, and members after `.`
  (also while the file doesn't parse). In JSX: tag names after `<` (the elements of the JSX
  runtime's `JSX.IntrinsicElements` and the components in scope) and attribute names inside an
  opening tag (the element's attributes or the component's props, minus those already written).
  After `</` the element still open there comes first. Tags are offered once the file contains
  JSX that parses, which is when its JSX runtime loads.
- **JSX**: go to definition, hover, references and rename on tags, opening and closing
  (`<Card` and `</Card>` → `function Card`), and on attributes (the attribute's or prop's
  declaration and type).
- **Signature help** while typing call arguments.
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
  `"a" + n` into a template literal, turn `if (count)` into `if (count !== 0)` (or `!== ""`,
  `!== null`, `!== 0.0`, by type), replace `export default` with a named export, add `await`
  or `spawn(...)` to a floating promise, and add `async` to a method whose promise must carry
  its errors. A fix that applies in several places is also offered as
  "Fix all in file", and **Fix all** (`source.fixAll`, e.g. on save) applies every preferred
  fix of the file.

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

## Visual Studio Code

The extension adds syntax highlighting and starts `velt lsp`.

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

The command **Velt: Restart Language Server** restarts it, for example after rebuilding `velt`.
For debugging configurations, see [Debugging](debugging.md#vs-code).

**Not yet available**: a published Marketplace extension, and packaged support for other
editors. Any editor that can start `velt lsp` as a language server works today.
