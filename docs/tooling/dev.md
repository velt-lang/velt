# `velt dev`: hot reload

```
velt dev [<file.vlt>] [--exe] [--locked] [-v] [-- <program args>...]
```

`velt dev` runs your program like `velt run`, then keeps it up to date while you edit. Save a
file and the running program picks up the change, usually with its in-memory state and open
connections intact.

```
$ velt dev server.vlt
velt dev: started in 410 ms
velt dev: hot-swapped 3 functions in 82 ms
velt dev: build failed (the previous version keeps running); waiting for changes
velt dev: restarted (Point gained a field) in 380 ms
```

## What happens on save

`velt dev` watches every file the build read (your modules, the standard library, path
dependencies) plus `package.vlt` and `velt.lock`, and new `.vlt` files next to them (so a module
that an import was missing is picked up as soon as you create it). Changes come from the
operating system's file notifications; where those don't work (some network or container file
systems), set `VELT_DEV_POLL=1` to check the files every 10 ms instead. A file saved while a
build is running leads to another build once it finishes. Files are compared by modification
time and length, so on file systems with coarse timestamps (FAT, some network mounts) two saves
of the same length within one timestamp tick can look like one. On a change it builds the new
version while the old one keeps running:

- **Hot swap** (the common case): the changed functions are compiled and swapped into the
  running program. In-memory data, open connections, caches and running tasks survive. New
  calls and requests run the new code; work already in flight finishes on the old code.
- **Restart**: when live state could not survive the edit, a new version starts instead, and
  the reason is printed. That happens when a struct or class layout changed, a closure's
  captures changed, a function's signature changed, `main` changed (it has already run), a
  function that live values may still call was removed, or closures were reordered.
- **Build error**: the diagnostics are printed and the old version keeps running.
- **Program exit**: `velt dev` prints the exit code and waits for the next change.

Stopping `velt dev` (Ctrl-C, or SIGTERM/SIGHUP from a process manager or `docker stop`) stops
the program the same way a reload does: it gets a stop request, can finish in-flight requests
for up to a second, and `velt dev` waits for it before exiting. Press Ctrl-C twice to exit at
once.

Listening sockets survive restarts: `velt dev` owns them and hands them to each version, so no
connection is refused during a reload and a server on port 0 keeps its port. Hot swap needs no
code changes because Velt has no mutable module-level state
([Variables](../reference/variables.md#no-mutable-module-state)): state lives in values that
`main` creates, so there are no globals to migrate.

## Modes

- **JIT** (the default): each version is compiled in memory with Cranelift and runs inside a
  `velt` host process. There is no link step and no new executable, which also avoids the
  operating system's first-launch checks of new binaries on macOS and Windows.
- **`--exe`**: each version is a linked debug executable (`target/velt/dev/<name>-<n>`), so
  you can attach a debugger to it ([Debugging](debugging.md#velt-dev-and-the-debugger)). Edits
  restart the program instead of hot-swapping. `--release`, `-g` and `--backend` are accepted
  with `--exe`.

`velt test --watch` reruns the tests on every change the same way ([Testing](../book/testing.md)).

## Speed

Save to first response with a new handler body, measured by `velt`'s reload benchmark
([bench/reload/RESULTS.md](../../bench/reload/RESULTS.md); Windows 11 x86_64, i9-12900HK,
release `velt`, other builds running on the machine):

| Mode | Median |
|---|---|
| JIT, hot swap | 64 ms |
| JIT, restart | 378 ms |
| `--exe`, restart | 2.6 s |

The time includes a 30 ms settle delay after the last write.

## Limits

- Code that is already running keeps running the old version: a future in flight (by design),
  the rest of `main`'s body, and an endless loop inside one async function. The functions they
  call do swap.
- Editing an async function that `main` awaits directly restarts the program. The call is
  already running (a server loop, say), and running code keeps its version, so a swap would
  have no visible effect; a restart runs the new code. To keep state across such edits, move
  the work into functions the long-running one calls (a request handler, a loop body): those
  swap.
- Edits to functions that only ran during startup are swapped, but they don't run again.
- After 200 hot swaps the host restarts to reclaim the memory of old code.
- JIT code has line-level debug information for GDB and LLDB on Linux; macOS is untested (LLDB
  needs `plugin.jit-loader.gdb.enable on`) ([Debugging](debugging.md#velt-dev-and-the-debugger)).
  On Windows, use `--exe` or a normal build to debug.
- JIT code registers its unwind information on Windows x64, macOS and Linux, so debuggers and
  backtraces walk through it; on Windows arm64 it does not yet. On musl (Alpine) the JIT host
  is unavailable, so use `--exe`. Hot swap is tested end to end on Windows x64 and
  Linux x86_64; macOS builds the same code, but the reload tests have not been run there yet.

How it works: [the hot reload design](../internals/design/hot-reload.md).
