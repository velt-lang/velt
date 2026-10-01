# Velt for Visual Studio Code

Language support for Velt (`.vlt` files; language reference: `docs/reference/` in the repository):

- Syntax highlighting (keywords, types, strings, template literals with `${}` substitutions,
  numbers with type suffixes such as `10u8` / `1.5f32`, comments).
- Brackets, comment toggling, auto-closing pairs.
- Through the language server (`velt lsp`):
  - diagnostics from the parser, the module loader and the type checker, as you type
    (unsaved changes of other open files are taken into account);
  - formatting (`Format Document`, uses `velt fmt`);
  - outline / document symbols;
  - go to definition (locals, functions, types, members of `this` and of typed locals, enum
    variants, imported names — also into other files);
  - hover (declaration signatures; the inferred type of locals and expressions when the program
    type-checks);
  - completion (locals, module items, imports, prelude items, keywords; members after `.`);
  - find references, rename, document highlight (reads and writes of the name under the cursor);
  - workspace symbols (`Ctrl+T`: declarations in the open programs and the workspace folders);
  - quick fixes (light bulb) for compiler errors: remove `mut`, `undefined` → `null`,
    `"a" + n` → a template literal, `if (count)` → `if (count !== 0)` (`""` / `null` / `0.0` by
    type); and for a promise left floating as a statement: add `await` or wrap it in `spawn(...)`;
  - inlay hints: inferred types of `const` / `let` / `for ... of` bindings and parameter names at
    call sites (toggle with `editor.inlayHints.enabled`);
  - signature help while typing call arguments (functions, methods, constructors, function-typed
    values);
  - semantic highlighting: types, functions, methods, parameters, properties, enum members;
    `let` bindings carry the `mutable` modifier (to underline them, add
    `"editor.semanticTokenColorCustomizations": { "rules": { "*.mutable:velt": { "underline": true } } }`).

## Requirements

The `velt` executable. Build it from the repository root:

```sh
cargo build --release -p veltc     # produces target/release/velt(.exe)
```

Put it on your `PATH`, or point the `velt.serverPath` setting at it, e.g.
`"velt.serverPath": "C:/src/velt/target/release/velt.exe"`. The standard library is found like the
CLI finds it (`VELT_STD`, or the `std/` directory of the checkout the executable was built in).

## Install

Requires Node.js 18+.

```sh
cd editors/vscode
npm install
npx tsc -p .                                                   # compile src/ → out/
npx @vscode/vsce package --allow-missing-repository --skip-license   # → velt-0.1.0.vsix
code --install-extension velt-0.1.0.vsix
```

(Or in VS Code: *Extensions* view → `...` → *Install from VSIX...*.)

For development, open `editors/vscode` in VS Code and press F5 to launch an Extension Development
Host with the extension loaded (run `npx tsc -watch -p .` alongside).

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `velt.serverPath` | `velt` | The `velt` executable (`~` and `${workspaceFolder}` are expanded). Changing it restarts the server. |
| `velt.trace.server` | `off` | `messages` / `verbose` log the LSP traffic to the *Velt Language Server* output channel. |

Command: **Velt: Restart Language Server** (e.g. after rebuilding `velt` or changing a package's
dependencies, which the server installs once per session).
