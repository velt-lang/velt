# Design: fast dev loop and hot reload (`velt dev`)

Status: implemented (phases 0–3). User documentation: [`velt dev`](../../tooling/dev.md).

Goal: building applications in Velt should feel like Node with `--watch` or Vite. You save a
file, and the running backend reflects the change in well under 100 ms. When possible it keeps
its in-memory state and open connections, like Dart/Flutter hot reload. Everything here is
dev-only; `velt build` and `velt run` output does not change.

## Starting point

Before this work, the time from `velt run examples/http_hello.vlt` to the first HTTP response
was about 240 ms on an Apple M4 (Node: about 50 ms):

| Step | Time |
|---|---|
| parse, sema, lower, optimize (std included) | ~8 ms |
| Cranelift code generation | ~3 ms |
| link (`cc` plus the runtime library) | ~40 ms |
| macOS first-launch check of the new executable | ~200 ms |
| process start to listening | ~20 ms |

The compiler was not the bottleneck; producing and launching a new executable was.

## Phases

Each phase is useful on its own, and the next builds on it.

### Phase 0: the macOS setting

Terminals listed under System Settings → Privacy & Security → Developer Tools skip the
first-launch check for programs they start. Documented in
[Platforms](../../tooling/platforms.md#macos-notes); `velt doctor` reports the first-launch time.

### Phase 1: watch, rebuild, restart, keep sockets open

`velt dev [<file>]` builds and runs like `velt run`, then stays up as a **supervisor**:

- It **watches** the files the loader actually read (plus `velt.toml` and `velt.lock`), not a
  directory glob, so imports into std or path dependencies are covered; a new `.vlt` file in one
  of their directories counts too (the module a failed build was missing). Changes come from
  OS notifications (the `notify` crate, each checked against the file's mtime and length), or
  from polling every 10 ms where notifications fail or `VELT_DEV_POLL=1`.
- It **rebuilds while the old version keeps serving.** On a compile error it prints the
  diagnostics and keeps the old process; on success it stops the old process and starts the new
  one.
- **Socket handover**: the supervisor owns the listening sockets, so a restart never refuses a
  connection; requests arriving during the swap wait in the kernel backlog. Ports are only known
  at run time, so the runtime asks the supervisor for them over a channel named by
  `VELT_DEV_SOCKET` (a Unix socket with `SCM_RIGHTS`; on Windows a named pipe carrying
  `WSADuplicateSocketW` records). The supervisor binds on the first request and returns the same
  socket to every later process.
- The old process gets a stop request, drains in-flight requests, and is killed after a short
  timeout. Interrupting the supervisor (Ctrl-C, SIGTERM, SIGHUP; console events on Windows)
  stops the program the same way and waits for it before exiting.

### Phase 2: JIT dev backend (no link, no new executable)

The supervisor starts a **host** child, `velt dev --host` (the same `velt` binary, so the
operating system checks it only once). The host compiles the program to VIR, JIT-compiles it with
Cranelift (the object and JIT backends share one module builder), and calls the program's entry
point directly.

- The runtime is linked into `velt` as a library behind a `host` feature, without its `main`.
  It exports a generated symbol table (`ABI_SYMBOLS`), checked against the runtime ABI contracts
  by a test and registered with the JIT.
- A separate process rather than the supervisor itself: runtime panics and `exit` end the
  process; the tokio runtime is a process-global with no shutdown; and there are process-wide
  statics (allocator, stdout buffers, panic hook). A crash ends only the host, and the
  supervisor starts a new one on the next save.
- On Windows, JIT code registers its unwind information (`RtlAddFunctionTable`), so debuggers and
  backtraces walk through it.

### Phase 3: hot swap while the program runs (state survives)

The host keeps running and replaces changed code in place. In-memory data, open connections,
caches and running tasks survive.

**A slot per function, plus trampolines.** cranelift-jit has no hot-swap mode, so Velt provides
its own: each reloadable function key gets a slot (an atomic code pointer in host memory) and a
tiny per-architecture trampoline that jumps through it. In dev builds every reference to an
**entry point** resolves to its trampoline: direct calls, function addresses (vtable slots,
closure code pointers, function values). Each reload JIT-compiles the changed functions into a
new module and stores the new code pointers into the slots. Old code is never freed during a
session; after many swaps the host restarts to reclaim it.

**Continuations are pinned to the version that created their state.** An async function's
`poll`/`drop` functions depend on its state layout, which comes from liveness analysis, so
almost any edit to the body changes it; and a parent state machine embeds the states of the
children it awaits directly. So `poll`/`drop` never go through trampolines: a future records its
version's `poll`/`drop` in its header when it is created, and a parent's `poll` calls the child
`poll` of its own version. Changing a child therefore recompiles the parents that embed it.
Effect: after a save, **new** calls (new requests, newly spawned tasks) run the new code, and
futures already in flight finish on the old code. That is the right behavior for a server.

**Which edits swap and which restart.** The host has the old and new VIR in memory and compares
them per function key. It restarts when an existing aggregate layout changed (live objects have
the old shape), a closure's environment layout changed, a function's signature changed (old
frames would call the new code with the old ABI), `main`'s already-executed part changed, a key
that live data may still reference disappeared, or the key diff is ambiguous. Everything else
swaps: function bodies, handler logic, new functions and types, string and constant changes, and
async bodies (through pinning). Velt has no mutable globals, so there is **no global state to
migrate**, unlike most hot-reload systems.

**Stable function keys.** A function's key must not depend on compilation order: generic
instances spell out their type arguments (`sum<i64>`), glue is keyed by its type's name, and
closures are `<fn>::{closure#N}` (stable while the closures in a function keep their order).

### Phase 4 (only if measurements need it): keep the front end warm

The long-lived host could keep parsed and type-checked std and prelude modules and re-run only
changed modules. Sema is now most of a reload's time (see "Measured"), so this is the next lever.

## Decisions

1. **Stable function keys**: symbols never depend on type-interning order.
2. **Language rule: no mutable module-level state.** Module scope holds only constants,
   functions and types; mutable shared state lives in `shared(...)` or class instances owned by
   `main` or handlers. This is what makes swapping migration-free
   ([the Reference](../../reference/variables.md#no-mutable-module-state)).
3. **Runtime rule: no cached code pointers.** Only vtables (through relocations) and future
   headers store code addresses; the HTTP handler descriptor is replaced per server on a swap,
   and every runtime API that takes callbacks must follow the same pattern.
4. **Futures stay pinned to their version**: typed errors kept `poll`/`drop` in the future
   header.
5. The supervisor also serves `velt test --watch`.
6. Handler state (closure environments, such as `shared` counters) survives a hot swap when the
   environment layout is unchanged, the common case; an end-to-end reload test checks it.

## As built: phase 3

**Stable keys** (`velt_vir/src/lower/keys.rs`): a function's key is its symbol, and no symbol
contains a type-table index. Generic instances append the type arguments as written
(`sum<i64>` demangles back); glue names its type the same way; closures stay
`<fn>::{closure#N}`; a repeated symbol gets `$dup<n>` and such keys are "ambiguous". A unit test
lowers the same program with a shifted type table and gets the same symbols.

**No lowering hook was needed.** The Cranelift module builder takes a naming policy. A dev
version declares, per function, a *reference* (what calls and function constants resolve to)
and optionally *code* to define:

- **Entry points** (everything not pinned): the reference is the key's trampoline, defined by
  the first version that has the key; the code is `<key>$impl`. Trampolines hold the absolute
  address of their slot, so they need no relocation and can be any distance away:
  `movabs r11, slot; jmp [r11]` on x86_64 (13 bytes), `ldr x16, lit; ldr x16, [x16]; br x16`
  plus a literal on aarch64 (24 bytes).
- **Pinned** functions (`$poll`, `$drop`, a handler's `$init`, the join glue): the reference is
  the code itself; a later version that does not recompile one imports the newest address.
- The handler descriptor's `init` is pinned too: `init`, `poll`, `drop` and the state size must
  come from one version. When a swap recompiles a handler, each running server gets a whole new
  descriptor and keeps its environment. For `velt:http`'s `serve`, the user's handler is a
  closure *value* that the std wrapper calls through its trampoline, so editing a handler never
  touches the descriptor.

**Classification** (`velt_codegen_cl/src/dev/`): each function key gets a 128-bit fingerprint of
its code that does not depend on ids (async state aggregates are fingerprinted by name, so code
embedding a child's state is recompiled through its reference to the child's pinned `poll`
instead). The restart rules, in order, each with its message: an ambiguous key changed; a layout
lost a structure and gained another (`Point gained a field`, `the captures of
main::{closure#0} changed`); a signature changed (`the signature of greet changed`); one of
`main`'s keys changed (`main changed (it already ran)`); a key whose address was taken
disappeared; closures were added or reordered while an existing one changed. Otherwise the
changed and new keys are compiled, plus every function that refers to a recompiled pinned
function. After 200 swaps the host restarts to reclaim memory.

**Supervisor and host** ([rt_abi_async.md §13](../contracts/rt_abi_async.md)): after `go`, the
host keeps its build-report connection; on a change the supervisor sends `reload` there, the
host builds on a background thread while the program runs, and answers `swapped <n>`,
`restart <reason>` or `failed`. The front end runs twice for a restart (once in the running
host to decide, once in the new host). After a reload that restarted, the same kind of edit
likely follows, so the next host starts together with the next `reload` request instead of
after the answer: both builds run in parallel, and on `swapped` or `failed` the spare host is
killed before it ran any user code. After a swap no spare starts: a second front end beside
the running host's only slows the swap down. The spare prints nothing about its build
(`VELT_DEV_QUIET`; the running host reports the same build), and the supervisor tells its
build report from a stale one by the peer's process id (`SO_PEERCRED`, `LOCAL_PEERPID`). Not on
Windows, where a spare on every save made hot swaps about 4× slower and restarts no faster
(process creation and the on-access scan cost more than the overlap saves), nor on Unixes
without a peer pid.

**Runtime audit**: the only stored code pointers are vtables, future headers and future wrappers
(`spawn`, `Promise.all`, `block_on`, pinned with their state), and the per-server HTTP handler
slots. Timers are tasks; WebSockets, child processes, fs and net take no callbacks.

**Platforms**: hot swap is tested end to end on Windows x64, where each version registers its own
unwind table (aligned to 16 bytes: an odd base address made Windows read the unwind data as a
chained entry). macOS and Linux (aarch64 and x86_64) compile from the same code, and the
trampoline bytes are unit-tested per architecture; the reload tests also pass on Linux x86_64
(not yet run on macOS). JIT frames there have no registered unwind information yet.

**Tests**: `velt_codegen_cl` `tests/hot_swap.rs` (hand-built VIR: old code reaching new code
through direct calls, function values and a pinned entry; restart reasons; repeated swaps);
`veltc` `dev/swap/tests.rs` (real sources: body, async-body and generic edits swap; layout,
signature, `main` and capture changes restart); and the reload tests in `tests/reload/` (JIT and
`--exe`, with a probe asserting that no connection is refused): swaps for `hello_server`,
`shared_counter` (the count continues), `imported_module`, `new_function`, `generic_edit`, a
fix after a failed build; restarts for `struct_field`, `signature_change`, `closure_captures`;
and `in_flight` (a slow request started before the swap answers from the old code while new
requests get the new code).

## Measured

Save to first new response ([bench/reload/RESULTS.md](../../../bench/reload/RESULTS.md),
Windows x64, release `velt`, a shared machine): hot swap **64 ms** median; JIT restart 378 ms;
`--exe` restart 2.6 s. Of a reload, sema is now the largest part.

Starting the next host together with the `reload` request after a restart (Linux x86_64,
release `velt`): JIT restart 107–114 → 93–95 ms, hot swap unchanged
([RESULTS.md](../../../bench/reload/RESULTS.md#spare-host-linux-x86_64-2026-10-01)).

## Known gaps

- Code that is already running keeps running old code: a future in flight (by design), `main`'s
  remaining body, an endless loop inside one async function. The functions they call do swap.
- Editing an async function that `main` awaits directly restarts (`main changed`): `main`'s
  state embeds the child's, so `main::$poll` refers to the child's pinned `poll` and is
  recompiled with it. Swapping instead is possible but useless: the running `main` future and
  the child's state inside it stay pinned to their version, and `main` never creates the child
  again, so the edit would only reach other callers. Migrating the in-flight child's state to
  the new layout is not possible in general (the layout comes from liveness analysis). The
  restart is therefore the behavior that makes the edit take effect; the user documentation
  says to move work that should swap into functions the long-running one calls.
- Edits to functions that only ran during startup are swapped but don't run again.

Out of scope: browser-side hot module replacement for frontends, and production zero-downtime
deploys (the socket handover could be reused for that later).
