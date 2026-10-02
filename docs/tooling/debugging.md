# Debugging

Velt executables are native programs with standard debug info, so any native debugger works:
LLDB or CodeLLDB on macOS and Linux, GDB on Linux, the Visual Studio debugger on Windows.
Breakpoints go on `.vlt` lines, and the call stack shows Velt function names.

## Which build has line information

| Build | Line info (`.vlt` file:line) | Use it for |
|---|---|---|
| `velt build --backend llvm` | yes, unoptimized | stepping, breakpoints, locals |
| `velt build --release -g` | yes, optimized (LLVM when clang is installed) | profiling, crash addresses |
| `velt build` / `velt run` (Cranelift, the debug default) | yes on Linux; macOS untested; function symbols on Windows | stepping, breakpoints, backtraces |
| `velt dev --exe` | as `velt build` | attaching to a running version |
| `velt dev` (JIT) | yes with GDB or LLDB on Linux; macOS untested (LLDB needs `plugin.jit-loader.gdb.enable on`); none on Windows | breakpoints and stepping in the running program |

The LLVM backend needs clang ([Platforms](platforms.md#prerequisites)). On Windows the linker
writes a `.pdb` next to the `.exe` (CodeView); elsewhere the debug info is DWARF. On macOS it
stays in the object file next to the executable (`target/velt/<name>.o`), so keep it there, or
run `dsymutil target/velt/<name>` to bundle it.

Cranelift builds carry line tables only: breakpoints, stepping and backtraces work by `.vlt`
line, but locals are not shown (use `--backend llvm` for those). On Windows they have function
symbols only.

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

By default `velt dev` runs JIT-compiled code inside a `velt` process (`velt dev --host`, started
by the `velt dev` you typed). On Linux each version announces its line tables to GDB and LLDB
(the GDB JIT interface), so a debugger attached to that process stops on breakpoints in `.vlt`
files and steps by line. macOS is untested; LLDB on Apple platforms reads JIT line tables only
after `settings set plugin.jit-loader.gdb.enable on`.

```sh
gdb -p "$(pgrep -f 'velt dev --host')"      # then: break app.vlt:12, continue
lldb -p "$(pgrep -f 'velt dev --host')"     # then: breakpoint set -f app.vlt -l 12, continue
```

To stop at the very start, run the host under the debugger instead (it runs the program once,
without watching for changes): `gdb -ex 'set breakpoint pending on' -ex 'break app.vlt:12' -ex run
--args velt dev --host app.vlt`. Hot-swapped functions are announced too. On Windows, or to attach
from VS Code:

- run `velt dev --exe app.vlt` (or the task *velt: dev --exe (current file)*): each version is a
  linked executable, and *Velt: attach to a running program* attaches to the current one (after
  a reload, attach again to the new process);
- or stop `velt dev` and use *Velt: debug current file*.

## WebAssembly

`velt build --target wasm32-wasip1` debug builds carry DWARF: `wasmtime run -D debug-info
target/velt/app.wasm` exposes it to LLDB or GDB attached to wasmtime:

```sh
gdb -ex 'set breakpoint pending on' -ex 'break app.vlt:12' -ex run \
    --args wasmtime run -D debug-info -O opt-level=0 target/velt/app.wasm
```

With wasmtime 49 and GDB 15, breakpoints, stepping and backtraces work by `.vlt` line. LLDB 18
stops on breakpoints and shows the backtrace, but stepping in wasm code aborts wasmtime; use GDB
to step. Browser builds
(`wasm32-unknown-unknown`) can be stepped in Chrome DevTools with the *C/C++ DevTools Support
(DWARF)* extension.
