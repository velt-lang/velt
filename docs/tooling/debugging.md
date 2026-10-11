# Debugging

Velt executables are native programs with standard debug info, so any native debugger works:
LLDB or CodeLLDB on macOS and Linux, GDB on Linux, the Visual Studio debugger on Windows.
Breakpoints go on `.vlt` lines, and the call stack shows Velt function names. In VS Code, F5
builds and debugs with nothing to configure ([VS Code](#vs-code)).

## Which build has line information

| Build | Line info (`.vlt` file:line) | Variables | Use it for |
|---|---|---|---|
| `velt build` (Cranelift, the debug default) | yes on Linux and macOS; on Windows function symbols only | yes on Linux and macOS | stepping, breakpoints, backtraces, inspecting values |
| `velt run`, `velt test` | as `velt build` | no (values stay in registers, which is faster) | running and testing |
| `velt build --backend llvm` | yes, unoptimized | no | stepping, breakpoints, backtraces |
| `velt build --release -g` | yes, optimized (LLVM when clang is installed) | no | profiling, crash addresses |
| `velt dev --exe` | as `velt build` | as `velt build` | attaching to a running version |
| `velt dev` (JIT) | yes with GDB or LLDB on Linux; macOS untested (LLDB needs `plugin.jit-loader.gdb.enable on`); none on Windows | no | breakpoints and stepping in the running program |

The LLVM backend needs clang ([Platforms](platforms.md#prerequisites)). On Windows the linker
writes a `.pdb` next to the `.exe` (CodeView); elsewhere the debug info is DWARF. On macOS it
stays in the object files next to the executable (`target/velt/<name>.o`, plus `<name>.cgu1.o`,
… when a large program is split into codegen units), so keep them there, or run `dsymutil
target/velt/<name>` to bundle it.

Debug builds by `velt build` (what F5 debugs) and `velt dev --exe` also describe each
function's parameters and local variables with their source types: a debugger shows `count`
as an `i64`, a class value as a pointer whose fields it expands (`p->x`), an enum by its member
name, and `NULL` for an empty `T | null` of a class. LLDB with Velt's formatters (which the VS
Code extension loads, [Command line](#command-line) shows how) also shows a string as its text,
an array as `len=N` with its elements, a union by its active member, and other options as
`null` or their value; without them those are raw words. Variables of `async` functions,
generators and inlined code are not shown yet. Variables are visible in the
whole function, also before their declaration runs. On Windows, Cranelift builds have function
symbols only. On macOS, executables are signed ad hoc without the hardened runtime, so
LLDB can launch and attach to them.

Panics print `panic: <message> at <file>:<line>:<col>` even without a debugger, and exit with
code 101 (there is no unwinding, so `RUST_BACKTRACE` does not apply).

## Command line

```sh
velt build app.vlt
lldb target/velt/app
(lldb) command script import <velt>/share/velt/lldb/velt_lldb.py
(lldb) breakpoint set -f app.vlt -l 12
(lldb) run
(lldb) frame variable
```

The `command script import` loads the formatters; `velt doctor` prints the script's path (the
*debugger scripts* line), and `velt build --json` reports it as `lldbScript`. To load them in
every session, put the line in `~/.lldbinit`.

GDB: `gdb target/velt/app`, then `break app.vlt:12` and `run`.

## VS Code

1. Install the Velt extension ([Editors](editors.md#visual-studio-code)) and a debugger extension:
   **CodeLLDB** (`vadimcn.vscode-lldb`, recommended: it includes LLDB), **LLDB DAP**
   (`llvm-vs-code-extensions.lldb-dap`) or **C/C++** (`ms-vscode.cpptools`). Without one, F5
   offers to install CodeLLDB.
2. Press **F5** in a package (a folder with `package.vlt`), or with a `.vlt` file open outside
   one. Or use **▶ Run | Debug** above `main`, or the run and debug buttons of the editor title.

The extension runs `velt build --json` (in the default, Cranelift build, so clang is not
needed), shows build errors in the *Problems* view, and starts the debugger on the executable
the build reports. It picks the first installed of CodeLLDB, LLDB DAP and C/C++; the
`velt.debug.engine` setting chooses one.

**Windows:** the default build has no line information yet, so breakpoints in `.vlt` files do
not bind (the extension warns). With clang installed, add `"buildArgs": ["--backend", "llvm"]` to
the configuration: that build has line information (a `.pdb`), which the C/C++ extension's
Visual Studio debugger reads.

A `launch.json` is optional. `velt new` and `velt init` write one, `velt init --editor vscode`
adds one to an existing package (it keeps files that exist), and so does **Velt: Generate
launch.json**:

```json
{
  "version": "0.2.0",
  "configurations": [{ "type": "velt", "request": "launch", "name": "Debug" }]
}
```

A `velt` configuration takes:

| Attribute | Meaning |
|---|---|
| `file` | build and debug this file (e.g. `"${file}"`) instead of the package |
| `program` | the executable to debug (default: the one `velt build` makes) |
| `args`, `env`, `cwd` | the program's arguments, environment and working directory |
| `build` | `false` skips `velt build` (then set `program`) |
| `buildArgs` | extra `velt build` options, e.g. `["--backend", "llvm"]` |
| `stopOnEntry` | stop before `main` runs |

`"request": "attach"` with `"pid": "${command:pickProcess}"` attaches to a running program; the
picker lists Velt programs (built into `target/velt/`) first.
`editors/vscode/templates/` has a `launch.json` with each kind and a `tasks.json` for
`velt dev --exe`.

### `velt dev` and the debugger

By default `velt dev` runs JIT-compiled code inside a `velt` process (`velt dev --host`, started
by the `velt dev` you typed). On Linux each version announces its line tables to GDB and LLDB
(the GDB JIT interface), so a debugger attached to that process stops on breakpoints in `.vlt`
files and steps by line. macOS is untested; LLDB on Apple platforms reads JIT line tables only
after `settings set plugin.jit-loader.gdb.enable on`.

Building that debug information costs little: about 1 ms for a 300-function program and 20 ms for
a 22,000-function one, under 2% of the time to the first run
([measurements](../../bench/RESULTS.md)). To turn it off, set `VELT_DEV_DEBUG_INFO=0`.

```sh
gdb -p "$(pgrep -f 'velt dev --host')"      # then: break app.vlt:12, continue
lldb -p "$(pgrep -f 'velt dev --host')"     # then: breakpoint set -f app.vlt -l 12, continue
```

To stop at the very start, run the host under the debugger instead (it runs the program once,
without watching for changes): `gdb -ex 'set breakpoint pending on' -ex 'break app.vlt:12' -ex run
--args velt dev --host app.vlt`. Hot-swapped functions are announced too. On Windows, or to attach
from VS Code:

- run `velt dev --exe app.vlt` (or the task *velt: dev --exe (current file)* of
  `editors/vscode/templates/tasks.json`): each version is a linked executable, and an attach
  configuration (`"request": "attach"`) attaches to the current one (after a reload, attach again
  to the new process);
- or stop `velt dev` and press F5.

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
