# Velt for Visual Studio Code

Language support for Velt (`.vlt` files; see the [language reference](https://github.com/velt-lang/velt/tree/main/docs/reference)):

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
    `"a" + n` → a template literal; and for a promise left floating as a statement: add `await`
    or wrap it in `spawn(...)`;
  - inlay hints: inferred types of `const` / `let` / `for ... of` bindings and parameter names at
    call sites (toggle with `editor.inlayHints.enabled`);
  - signature help while typing call arguments (functions, methods, constructors, function-typed
    values);
  - semantic highlighting: types, functions, methods, parameters, properties, enum members;
    `let` bindings carry the `mutable` modifier (to underline them, add
    `"editor.semanticTokenColorCustomizations": { "rules": { "*.mutable:velt": { "underline": true } } }`);
  - **▶ Run | Debug** above `main`, and **▶ Run test | Debug test** above each test.
- Debugging (F5): breakpoints in `.vlt` files, stepping and the call stack, with nothing to
  configure. See [Debugging](#debugging).

## Requirements

The Velt toolchain. Install it with one line (Linux x86_64 and arm64, macOS, Windows x64):

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh   # Linux, macOS
irm https://github.com/velt-lang/velt/releases/latest/download/get-velt.ps1 | iex       # Windows (PowerShell)
```

The installer puts `velt` on your `PATH`; `velt doctor` checks the setup. If `velt` lives
elsewhere, point the `velt.serverPath` setting at it. Other options:
[Platforms and installation](https://github.com/velt-lang/velt/blob/main/docs/tooling/platforms.md).

## Debugging

Install a debugger extension: [CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-lldb)
(recommended; it includes LLDB), LLDB DAP, or Microsoft's C/C++. Then press **F5** in a package
(a folder with `package.vlt`) or on an open `.vlt` file: the extension runs `velt build --json`,
shows build errors in *Problems*, and starts the debugger on the program. No `launch.json` is
needed; **Velt: Generate launch.json** (or `velt init --editor vscode`) writes one to customize
(`args`, `env`, `cwd`, `program`, `file`, `build`, `buildArgs`, `stopOnEntry`):

```json
{ "type": "velt", "request": "launch", "name": "Debug", "args": ["--port", "8080"] }
```

`velt new` already adds it. On Windows use CodeLLDB or LLDB DAP; the C/C++ extension's Visual
Studio debugger needs `"buildArgs": ["--backend", "llvm"]` (needs clang), whose build has a PDB. [`templates/`](https://github.com/velt-lang/velt/tree/main/editors/vscode/templates) has examples, including
attaching to a program that `velt dev --exe` restarts on every change.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `velt.serverPath` | `velt` | The `velt` executable (`~` and `${workspaceFolder}` are expanded). Changing it restarts the server. |
| `velt.trace.server` | `off` | `messages` / `verbose` log the LSP traffic to the *Velt Language Server* output channel. |
| `velt.debug.engine` | `auto` | The debugger extension F5 starts: `codelldb`, `lldb-dap`, `cpptools`, or `auto` (the first installed, in that order). |

Commands: **Velt: Restart Language Server** (e.g. after rebuilding `velt` or changing a package's
dependencies, which the server installs once per session), **Velt: Run File**, **Velt: Debug
File** (also as buttons in the editor title) and **Velt: Generate launch.json**.

## Development

The extension lives in [`editors/vscode`](https://github.com/velt-lang/velt/tree/main/editors/vscode)
of the Velt repository. With Node.js 18+:

```sh
cd editors/vscode
npm install
npx tsc -p .                        # compile src/ → out/
npx @vscode/vsce package            # → velt-<version>.vsix
code --install-extension velt-<version>.vsix
```

Or open `editors/vscode` in VS Code and press F5 to launch an Extension Development Host (run
`npx tsc -watch -p .` alongside). `npm test` runs the unit tests of the debug configuration logic
(`src/test/`).
