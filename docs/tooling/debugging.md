# Debugging

Velt executables are native programs with standard debug info, so any native debugger works:
LLDB or CodeLLDB on macOS and Linux, GDB on Linux, the Visual Studio debugger on Windows.
Breakpoints go on `.vlt` lines, and the call stack shows Velt function names.

## Which build has line information

| Build | Line info (`.vlt` file:line) | Use it for |
|---|---|---|
| `velt build --backend llvm` | yes, unoptimized | stepping, breakpoints, locals |
| `velt build --release -g` | yes, optimized (LLVM when clang is installed) | profiling, crash addresses |
| `velt build` / `velt run` (Cranelift, the debug default) | function symbols only | backtraces |
| `velt dev --exe` | function symbols only | attaching to a running version |
| `velt dev` (JIT) | none: the program runs inside the `velt` process | not debuggable as a program |

The LLVM backend needs clang ([Platforms](platforms.md#prerequisites)). On Windows the linker
writes a `.pdb` next to the `.exe` (CodeView); elsewhere the debug info is DWARF. On macOS it
stays in the object file next to the executable (`target/velt/<name>.o`), so keep it there, or
run `dsymutil target/velt/<name>` to bundle it.

Panics print `panic: <message> at <file>:<line>:<col>` even without a debugger, and exit with
code 101 (there is no unwinding, so `RUST_BACKTRACE` does not apply).

## Command line

```sh
velt build app.vlt --backend llvm
lldb target/velt/app
(lldb) breakpoint set -f app.vlt -l 12
(lldb) run
```

GDB: `gdb target/velt/app`, then `break app.vlt:12` and `run`.

## VS Code

1. Install the Velt extension ([Editors](editors.md#visual-studio-code)). It enables breakpoints
   in `.vlt` files for every debugger.
2. Install a debugger extension: **CodeLLDB** (`vadimcn.vscode-lldb`) on macOS and Linux, or the
   **C/C++** extension (`ms-vscode.cpptools`, which provides `cppvsdbg`) on Windows.
3. Copy `editors/vscode/templates/launch.json` and `tasks.json` into your project's `.vscode/`.

| Configuration | What it does |
|---|---|
| *Velt: debug current file* | runs `velt build <file> --backend llvm`, then launches `target/velt/<stem>` under the debugger |
| *Velt: debug package* | `velt build --backend llvm` in the package root, then launches `target/velt/<folder name>`; if the package name differs from the folder name, edit `program` |
| *Velt: attach to a running program* | pick a process, for example a `velt run` program or the current version of a `velt dev --exe` session |

Build errors from the tasks appear in the *Problems* view.

### `velt dev` and the debugger

By default `velt dev` runs JIT-compiled code inside a `velt` process, which has no debug info for
your program. To debug while iterating:

- run `velt dev --exe app.vlt` (or the task *velt: dev --exe (current file)*): each version is a
  linked executable, and *Velt: attach to a running program* attaches to the current one (with
  function-level symbols; after a reload, attach again to the new process);
- for line-level debugging, stop `velt dev` and use *Velt: debug current file*.

## WebAssembly

`velt build --target wasm32-wasip1` debug builds carry DWARF: `wasmtime run -D debug-info
target/velt/app.wasm` exposes it to LLDB or GDB attached to wasmtime. Browser builds
(`wasm32-unknown-unknown`) can be stepped in Chrome DevTools with the *C/C++ DevTools Support
(DWARF)* extension.
