# Editors and the language server

`velt lsp` is a Language Server Protocol server on stdin/stdout. It works in any editor with an
LSP client; the repository ships a Visual Studio Code extension in `editors/vscode`.

## Features

- **Diagnostics** from the parser, the module loader and the type checker, as you type. Unsaved
  changes of other open files are taken into account.
- **Formatting** with [`velt fmt`](fmt.md).
- **Navigation**: document outline, go to definition (also through `ns.x` namespace imports and
  re-exports, landing on the original declaration), find references, rename, document highlight
  (reads and writes), workspace symbols.
- **Hover**: declaration signatures, including inferred `throws` types, and the inferred type of
  locals and expressions.
- **Completion**: locals, module items, imports, prelude items, keywords, and members after `.`
  (also while the file doesn't parse). In JSX: tag names after `<` (the elements of the JSX
  runtime's `JSX.IntrinsicElements` and the components in scope) and attribute names inside an
  opening tag (the element's attributes or the component's props, minus those already written).
  Tags are offered once the file contains JSX that parses, which is when its JSX runtime loads.
- **JSX**: go to definition and hover on component tags (`<Card` → `function Card`) and on
  attributes (the attribute's or prop's declaration and type).
- **Signature help** while typing call arguments.
- **Inlay hints**: inferred types of `const` / `let` / `for...of` bindings and parameter names at
  call sites.
- **Semantic highlighting**: types, functions, methods, parameters, properties and enum members;
  `let` bindings carry a `mutable` modifier you can style.
- **Quick fixes** for compiler errors: remove `mut`, replace `undefined` with `null`, turn
  `"a" + n` into a template literal, turn `if (count)` into `if (count !== 0)` (or `!== ""`,
  `!== null`, `!== 0.0`, by type), replace `export default` with a named export, and add `await`
  or `spawn(...)` to a floating promise.

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
