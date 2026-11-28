# Fast dev loop and hot reload (`velt dev`)

Status: implemented.

## Summary

User documentation: [`velt dev`](https://veltlang.com/docs/tooling/dev/).

`velt dev` makes building an application in Velt feel like Node with `--watch` or Vite: save a
file, and the running backend reflects the change in well under a second, usually without
losing its in-memory state or its open connections, like Dart/Flutter hot reload. A supervisor
watches the sources and owns the listening sockets; a host process JIT-compiles the program
with Cranelift and, on each save, swaps the changed functions into the running program or
restarts it when live data could meet code built for another shape of it. Everything here is
dev-only: `velt build` and `velt run` output does not change.

## Motivation

The compiler is rarely the slow part of an edit-run cycle; producing and launching a new
executable is. `velt run` links every build with the system linker and the runtime library, and
on macOS every new executable pays the operating system's first-launch check (hundreds of
milliseconds, more under load). Restarting also drops every connection and all in-memory state,
which for a server under development means re-creating sessions, caches and test data after
every save.

## Design

The design is layered in phases; each is useful on its own, and the next builds on it.

### Phase 0: the macOS setting

Terminals listed under System Settings → Privacy & Security → Developer Tools skip the
first-launch check for programs they start. This is documented in
[Platforms](https://veltlang.com/docs/tooling/platforms/#macos-notes), and `velt doctor` reports
how long a new executable takes to start (`crates/veltc/src/commands/doctor/smoke.rs`).

### Phase 1: watch, rebuild, restart, keep sockets open

`velt dev [<file>]` builds and runs like `velt run`, then stays up as a **supervisor**:

- It **watches** the files the loader actually read (plus `package.velt` and the lockfile), not a
  directory glob, so imports into std or path dependencies are covered. A new source file
  (`.velt`, `.ts`, `.tsx`) in one of their directories counts too: it may be the module a failed
  build was missing. Changes come from OS notifications (the `notify` crate, each checked against
  the file's modification time and length), or from polling every 10 ms where notifications are
  unavailable or `VELT_DEV_POLL=1`. A change is reported once the files have been quiet for 30 ms
  (`watch::SETTLE`), so an editor's write-rename-touch sequence triggers one rebuild
  (`crates/veltc/src/dev/watch/`).
- It **rebuilds while the old version keeps serving.** On a compile error it prints the
  diagnostics and keeps the old program; on success it replaces it.
- **Socket handover**: the supervisor owns the listening sockets, so a restart never refuses a
  connection; requests arriving during the swap wait in the kernel backlog. Ports are known only
  at run time, so the runtime asks the supervisor for them over a channel named by
  `VELT_DEV_SOCKET` (a Unix socket carrying the listener with `SCM_RIGHTS`; on Windows a named
  pipe carrying `WSADuplicateSocketW` records). The supervisor binds on the first request and
  returns the same socket to every later process (`crates/veltc/src/dev/handover/`).
- The old process gets a stop request, drains in-flight requests, and is killed after a grace
  period of 1.5 s (`VELT_DEV_STOP_GRACE_MS` overrides it). Interrupting the supervisor (Ctrl-C,
  SIGTERM, SIGHUP; console events on Windows) stops the program the same way and waits for it
  before exiting.

The same supervisor serves `velt test --watch`.

### Phase 2: JIT dev backend (no link, no new executable)

The supervisor starts a **host** child, `velt dev --host` (the same `velt` binary, so the
operating system checks it only once). The host compiles the program to VIR, JIT-compiles it with
Cranelift (the object and JIT backends share one module builder), and calls the program's entry
point directly.

- The runtime is compiled a second time into `velt` as a library (`crates/velt_rt_host`, with
  `cfg(velt_rt_host)` removing its `main`). It exports a generated symbol table (`ABI_SYMBOLS`),
  checked against the runtime ABI contracts by a test and registered with the JIT.
- The host is a separate process rather than the supervisor itself because runtime panics and
  `exit` end the process, the executor in Velt is process-global with no shutdown, and there are
  process-wide statics (allocator, stdout buffers, panic hook). A crash ends only the host, and
  the supervisor starts a new one on the next save. A host that faults says where on stderr
  before it dies: on Unix one line for a `SIGSEGV` or a `SIGBUS` (macOS's signal for a write to
  a protected page) with the address touched, the instruction, the thread and the step of the
  reload it was in (`dev/fault_unix.rs`); on Windows x64 an access violation with registers
  and the stack (`dev/fault.rs`).
- Each version registers its unwind information with the system, so debuggers and backtraces
  walk through JIT frames: `RtlAddFunctionTable` on Windows x64 (`unwind/jit_windows.rs`), DWARF
  CFI through `__register_frame` on macOS and Linux (`unwind/jit_systemv.rs`). Line tables reach
  GDB and LLDB through the GDB JIT interface, an in-memory ELF image per version
  (`debug_info::jit`), so breakpoints on `.velt` lines stop in JIT code.
- JIT code is not position independent. Each module (each loaded version) allocates its code and
  data from one contiguous arena, so references inside it fit 32-bit PC-relative relocations.
  References to anything outside it (the runtime, C library functions, earlier versions) use
  absolute 64-bit addresses, because the host's own code can be gigabytes away.

### Phase 3: hot swap while the program runs (state survives)

The host keeps running and replaces changed code in place. In-memory data, open connections,
caches and running tasks survive.

**A slot per function, plus trampolines.** cranelift-jit has no hot-swap mode, so Velt provides
its own: each reloadable function key gets a slot (an atomic code pointer in host memory) and a
small per-architecture trampoline that jumps through it. In dev builds every reference to an
**entry point** resolves to its trampoline: direct calls and function addresses (vtable slots,
closure code pointers, function values). Each reload JIT-compiles the changed functions into a
new module and stores the new code pointers into the slots. Old code is never freed during a
session; after 200 swaps the host restarts to reclaim it.

**Continuations are pinned to the version that created their state.** An async function's
`poll`/`drop` functions depend on its state layout, which comes from liveness analysis, so almost
any edit to the body changes it; and a parent state machine embeds the states of the children it
awaits directly. So `poll`/`drop` never go through trampolines: a future records its version's
`poll`/`drop` in its header when it is created, and a parent's `poll` calls the child `poll` of
its own version. Changing a child therefore recompiles the parents that embed it. After a save,
**new** calls (new requests, newly spawned tasks) run the new code, and futures already in flight
finish on the old code: the right behavior for a server.

**Which edits hot-swap.** The host has the old and new VIR in memory and compares them per
function key. It restarts when an existing aggregate layout changed (live objects have the old
shape), a closure's environment layout changed, a function's signature changed (old frames would
call the new code with the old ABI), `main`'s already-executed part changed, a key that live data
may still reference disappeared, closures were renumbered, an ambiguous key changed, or a value
would be represented differently (a class one version counts and the other does not, closures
that gain or lose an identity). Everything else swaps: function bodies, handler logic, new
functions and types, string and constant changes, and async bodies (through pinning). Module
state is never migrated: it is kept as it is, or started over (decision 2).

**Stable function keys.** A function's key does not depend on compilation order: generic
instances spell out their type arguments (`sum<i64>`), glue is keyed by its type's name, and
closures are `<fn>::{closure#N}` (stable while the closures in a function keep their order).

### Phase 4: a warm front end

The host is long-lived, so it can keep the work of the previous build instead of running the
whole front end again on every save. An edit of function bodies alone is checked, lowered and
optimized from the running version's kept state; see [the warm front end](dev-warm-front-end.md).

## Decisions

1. **Stable function keys**: symbols never depend on type-interning order.
2. **Module state is kept, or starts over; never migrated.** Module-level `let`s live in a
   process-wide block the runtime keys by the address of the program's module descriptor. A
   version whose module state has the same key as the running one (the descriptor's contents:
   size, slot names, init and drop functions, and the fingerprints of those functions, which
   spell the layout and the initializers) imports the running descriptor, so swapped-in code
   reads and writes the live values: every edit that leaves the module-level declarations alone
   keeps the state. An edit that changes an initializer but keeps the block's layout also swaps,
   into a fresh block: the version gets a descriptor of its own, its code a block initialized by
   its own initializers, and every function reaching module state is recompiled, changed or not,
   so all new calls use the new block. An edit that changes the layout (a module-level `let`
   added or removed, a slot whose type changes) restarts (`module state gained a slot`, `module
   state lost a slot`, `the type of a module state slot changed`), and so does a renamed slot:
   the descriptor holds the slot names, and `main` reaches the descriptor (`main changed (it
   already ran)`); see [the Reference](https://veltlang.com/docs/reference/variables/#module-state).
   State in `shared(...)` or in class instances owned by `main` or handlers carries over. So do a
   static atomic (`const X = new Atomic<T>(literal)`, not module state) and a `ThreadLocal`'s
   key: the session owns one word or key per constant, named by its qualified name and type,
   which every version imports (`crates/velt_codegen_cl/src/dev/mod.rs`); an edit changing the
   type gets a new one.
3. **Runtime rule: no cached code pointers.** Only vtables (through relocations) and future
   headers store code addresses, and runtime APIs that take callbacks keep them replaceable the
   same way ([rt_abi_async.md §13.5](../contracts/rt_abi_async.md)).
4. **Futures stay pinned to their version**: the future header holds the `poll`/`drop` of the
   version that created it.
5. **Handler state survives**: closure environments (such as `shared` counters) carry over a hot
   swap when the environment layout is unchanged, the common case.

## Compilation

**Stable keys** (`crates/velt_vir/src/lower/keys.rs`): a function's key is its symbol, and no
symbol contains a type-table index. Generic instances append the type arguments as written
(`sum<i64>` demangles back); glue names its type the same way; closures are
`<fn>::{closure#N}`; a repeated symbol gets `$dup<n>`, and such keys are "ambiguous". A unit test
lowers the same program with a shifted type table and gets the same symbols.

**Naming policy, not a lowering hook.** The Cranelift module builder takes a naming policy. A dev
version declares, per function, a *reference* (what calls and function constants resolve to) and
optionally *code* to define:

- **Entry points** (everything not pinned): the reference is the key's trampoline, defined by the
  first version that has the key; the code is `<key>$impl`. Trampolines hold the absolute address
  of their slot, so they need no relocation and can be any distance away:
  `movabs r11, slot; jmp [r11]` on x86_64 (13 bytes), `ldr x16, lit; ldr x16, [x16]; br x16`
  plus a literal on aarch64 (24 bytes) (`crates/velt_codegen_cl/src/dev/trampoline.rs`).
- **Pinned** functions (async `$poll` and `$drop`, the value wrappers' `$value_poll` and
  `$value_drop`, and the `Promise.all`/`allSettled` glue;
  `crates/velt_codegen_cl/src/dev/roles.rs`): the reference is the code itself; a later version
  that does not recompile one imports the newest address.

**Classification** (`crates/velt_codegen_cl/src/dev/classify.rs`; the fingerprint is
`velt_vir::fingerprint`, `Mode::HotReload`): each function key gets a 128-bit fingerprint of its
code that does not depend on ids (async state aggregates are fingerprinted by name, so code
embedding a child's state is recompiled through its reference to the child's pinned `poll`
instead). Before classification, the `veltc` host compares lowering's counted types and the weak
and proxy sets it records for this comparison (`CountedSeed::representation_change`: `class Node
is now counted`, `the identity of function values changed`). Then the restart rules run in order,
each with its message:

| Rule | Message |
|---|---|
| an ambiguous key changed | `` `k` has an ambiguous name `` |
| a layout name lost a structure and gained another | `Point gained a field`, `the captures of main::{closure#0} changed` |
| the module state's layout changed | `module state gained a slot` |
| a signature changed | `the signature of greet changed` |
| one of `main`'s keys changed | `main changed (it already ran)` |
| a key whose address was taken disappeared | `f was removed, but live values may still call it` |
| closures were added or reordered while an existing one changed | `closures in f were added or reordered` |

Otherwise the changed and new keys are compiled, plus every function that refers to a recompiled
pinned function, plus every function reaching module state when its key changed (its layout did
not: a changed layout restarts, see decision 2).

**Supervisor and host** ([rt_abi_async.md §13](../contracts/rt_abi_async.md)): after `go`, the
host keeps its build-report connection; on a change the supervisor sends `reload` there, the host
builds on a background thread while the program runs, and answers `swapped <n>`,
`restart <reason>` or `failed`. A restart runs the front end twice (once in the running host to
decide, once in the new host). After a reload that restarted, the same kind of edit likely
follows, so on Linux and Apple platforms the next host starts together with the next `reload`
request instead of after the answer: both builds run in parallel, and on `swapped` or `failed`
the spare host is killed before it ran any user code. After a swap no spare starts: a second
front end beside the running host's only slows the swap down. The spare prints nothing about its
build (`VELT_DEV_QUIET`; the running host reports the same build), and the supervisor tells its
build report from a stale one by the peer's process id (`SO_PEERCRED`, `LOCAL_PEERPID`). Windows
starts no spare: there a spare on every save made hot swaps about 4× slower and restarts no
faster, because process creation and the on-access scan cost more than the overlap saves.

**Runtime audit**: the only stored code pointers are vtables and future headers, plus the drop
glue kept with a value in flight (a join's or a spawned task's result), which matches a layout a
swap does not change. Callbacks in std modules are compiled Velt code, never function pointers the
runtime keeps.

**Tests**:
- `velt_codegen_cl` `src/tests/hot_swap.rs` (hand-built VIR: old code reaching new code through
  direct calls, function values and a pinned entry; restart reasons; repeated swaps) and
  `src/tests/hot_swap_state.rs` (which module state descriptor swapped code reaches);
- `veltc` `src/dev/swap/tests.rs` (real sources: body, async-body and generic edits swap; layout,
  signature, `main` and capture changes restart);
- the reload tests in `tests/reload/`, run by `crates/veltc/tests/reload.rs` in JIT and `--exe`
  mode, with a probe asserting that no connection is refused. Each case is a sequence of saves
  with the expected outcome: swaps that keep state (`shared_counter`'s count continues,
  `module_state_kept`), restarts with their reasons (`struct_field`, `signature_change`,
  `closure_captures`, `counted_change`, `fn_identity_change`), a fix after a failed build, and
  `in_flight` (a slow request started before the swap answers from the old code while new
  requests get the new code).

Hot swap runs on Windows x64, macOS and Linux (aarch64 and x86_64) from the same code; the
trampoline bytes are unit-tested per architecture. On Windows, each version's unwind table is
aligned to 16 bytes: Windows reads an odd base address's unwind data as a chained entry. JIT
frames on Windows arm64 have no registered unwind information.

## Runtime cost

Save to first new response is measured by `bench_save_to_first_response` in
`crates/veltc/tests/reload.rs`; results are in
[bench/reload/RESULTS.md](../../../bench/reload/RESULTS.md). With the cold front end, on an
Apple M4: hot swap 57 ms median, JIT restart 91 ms, `--exe` restart 419 ms. On Windows x64
(i9-12900HK, a shared machine): hot swap 64 ms, JIT restart 378 ms, `--exe` restart 2.6 s; process
creation is the difference. The 30 ms settle delay is part of every figure. Starting a spare host
with the `reload` request after a restart cuts a JIT restart from 107–114 ms to 93–95 ms on Linux
x86_64 and leaves hot swaps unchanged. For a large program the front end dominates a reload,
which the [warm front end](dev-warm-front-end.md) addresses.

Dev builds pay for the trampolines: every call to an entry point is an indirect jump through its
slot. `velt build` and `velt run` never use them.

## Limitations

- Code that is already running keeps running old code: a future in flight (by design), `main`'s
  remaining body, an endless loop inside one async function. The functions they call do swap.
- Editing an async function that `main` awaits directly restarts (`main changed`): `main`'s
  state embeds the child's, so `main::$poll` refers to the child's pinned `poll` and is
  recompiled with it. Swapping instead would be useless: the running `main` future and the
  child's state inside it stay pinned to their version, and `main` never creates the child again,
  so the edit would reach only other callers. Migrating the in-flight child's state to the new
  layout is not possible in general (the layout comes from liveness analysis). The restart is
  therefore the behavior that makes the edit take effect; the user documentation says to move
  work that should swap into functions the long-running one calls.
- Edits to functions that ran only during startup are swapped but do not run again.
- Out of scope: browser-side hot module replacement for frontends, and production zero-downtime
  deploys.
