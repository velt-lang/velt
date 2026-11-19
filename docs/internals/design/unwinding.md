# Velt's own unwinding

Status: partly implemented. Issue: [#2667](https://github.com/velt-lang/velt/issues/2667).

## Summary

A panic (`panic(msg)`, an index or key check, `unreachable`, a failed transfer) in a program that
catches panics is a real exception that Velt raises and Velt's generated code catches, with no
Rust frame involved:

- **Velt raises its own exception**, of class `VELTPANC` (`velt_vir::eh::CLASS`), from std code
  with the platform's unwinder: `_Unwind_RaiseException` on macOS and Linux (every Itanium-ABI
  target), `RaiseException` on Windows x64.
- **Generated code catches it.** The std-only intrinsic `catchUnwind(f)` calls `f` at a call site
  with a landing pad (LLVM `invoke`, Cranelift `try_call`) and returns the exception object when a
  Velt exception unwound out of `f`, 0 when `f` returned.
- **Only Velt's class is caught.** The personality routine claims `VELTPANC` and nothing else: a
  C++ exception, a Rust panic or any other foreign exception passes as if the frame had no
  landing pad.
- **Unwound frames drop what they own.** In a program that catches, calls in frames that own
  values get cleanup pads that drop them and resume the unwind
  ([Cleanup in unwound frames](#cleanup-in-unwound-frames)).
- **The executor in Velt catches at a started promise** with it: `catchPanic` (in `velt/task`)
  and an HTTP handler's 500 on a panic are built on `catchUnwind`.
- **Panics of Rust code stay fatal.** A Rust panic in `velt_rt` (a runtime bug) ends the process
  from the runtime's panic hook before any unwinding.
- **WebAssembly does not unwind.** A panic ends the instance; `catchUnwind` is a plain call that
  returns 0.

A program whose live code never calls `catchUnwind` is unaffected: it panics through
`velt_rt_panic` (print, exit 101), and its code has no `velt_eh_try`, no landing pads and no
cleanup pads.

## Motivation

A server must answer 500 when one request panics, not end the process. The executor that polls
tasks is std code ([#2649](https://github.com/velt-lang/velt/issues/2649)), so there is no Rust
frame above a task's poll that could `catch_unwind`. Generated code cannot catch a Rust panic
either: a Rust panic is an exception of class `MOZ\0RUST`, and only Rust may end one (its cleanup
routine aborts when a foreign frame deletes it). And the mechanism must not be new Rust: the
runtime is moving into std ([#2196](https://github.com/velt-lang/velt/issues/2196)). So Velt
raises and catches an exception of its own.

## Design

### What is raised, and by whom

The raise is std code (`std/lang/hooks/unwind.velt`, `panicRaise`). In a program that catches,
lowering calls this hook at every panic site where it would otherwise call `velt_rt_panic`.

```text
panicRaise(msg):
  if this thread runs a catchUnwind that catches (CATCHING > 0):
    flush stdout; print "panic: <msg>" to stderr
    if UNWINDING: print "panic while unwinding: aborting"; abort()
    UNWINDING = true
    raise(msg)            // claimed by the innermost live velt_eh_try; returns only if not
    UNWINDING = false
    exit 101              // fallback, as an uncaught panic
  velt_rt_panic(msg)      // print, exit 101
```

**Per target.** The raise is chosen by std's conditional import `#unwind-raise`
(`std/imports.velt`): `std/lang/hooks/unwind_itanium.velt` on native targets other than Windows,
`unwind_seh.velt` on Windows, and `unwind_none.velt` on WebAssembly, which raises nothing and
names no unwinder function. The backends name no `_Unwind_*` function on Windows or WebAssembly.

**The count.** `CATCHING` (a `ThreadLocal<u32>`) is above 0 exactly when a `velt_eh_try` frame
with a landing pad is live on this thread's stack. `enterCatch` first asks the backend:
`velt_eh_catches()` (`velt_vir::eh::CATCHES`, a constant function the backend defines next to
`velt_eh_try`) is 1 in objects for Itanium targets and Windows x64, and 0 in the JIT (`velt dev`,
generator hosts), on Windows on arm64 and on WebAssembly. Where it is 0, `catchVeltPanic` calls
`f` plainly and does not count, so a panic takes `velt_rt_panic` instead of raising an exception
nothing would claim.

The count exists for two reasons. On Itanium targets a Rust `catch_unwind` claims every exception
in the search phase (Rust's personality does not look at the class) and then aborts on a foreign
one; raising only when a Velt catch is the nearest catching frame keeps such a frame from ever
seeing a Velt exception. And an uncaught panic exits at once instead of walking the stack.

**A raise nobody claims.** `_Unwind_RaiseException` returns only when the search phase reaches
the end of the stack without a frame claiming the exception. Whether it gets there depends on the
frames above `main`, which Velt does not control: the runtime's entry functions are Rust
`extern "C"` functions, whose abort-on-unwind landing pads Rust's personality claims for any
class. On Windows an unclaimed `RaiseException` never returns, so the first raise installs an
unhandled-exception filter that exits with 101 ([Windows](#windows)). So `panicRaise`'s exit 101
after a returned raise is a fallback; the guarantee is that it raises only when a `velt_eh_try`
that catches is live.

**The exception object.** One `malloc`:

| offset | size | field |
|---:|---:|---|
| 0 | 8 | `exception_class` = `VELTPANC` (little-endian bytes) |
| 8 | 8 | `exception_cleanup` = null (the unwinder never deletes it; the catcher frees it) |
| 16 | 16 | the unwinder's private words (zeroed) |
| 32 | n+1 | the message, UTF-8, NUL-terminated |

The header size is `velt_vir::eh::HEADER_SIZE`. The object never leaves its thread: a catch and
its raise are on one stack. `takePanic(e)` reads the message and frees the object; `panicText(e)`
reads it without freeing.

**Rust code inside a catch.** Rust frames between the raise and the catch (a runtime function
that runs Velt code, the drop glue it calls) must be `extern "C-unwind"`: the foreign exception
then passes them and their own cleanups run. A Rust `extern "C"` function on that path aborts the
process ("panic in a function that cannot unwind"). The rule: every runtime function that can run
Velt code is `C-unwind` ([Limitations](#limitations) lists the function-pointer types that are
not yet).

**Panics of Rust code.** The runtime's panic hook ends the process before a Rust panic unwinds,
with `panic: <msg> (runtime: file:line)`. They are bugs, and catching them would hide that the
runtime's state is broken.

### The catch: `catchUnwind`

```ts
@intrinsic
export declare unsafe function catchUnwind(f: () => void): usize;
@intrinsic
export declare unsafe function catchUnwindAt(code: usize, arg: usize): usize;
```

Both are in `std/lang/intrinsics/unwind.velt` (`hir::Intrinsic::CatchUnwind`, `CatchUnwindAt`).
Lowering calls `velt_eh_try(code, env)`, a function the backend defines in the object
(`velt_vir::eh::TRY`; like `velt_tls_*`, no runtime function is behind the symbol). It returns
null when `code(env)` returned and the exception object when a Velt exception unwound out of it;
the caller owns the object. `catchUnwindAt` calls the function at an address (`fnAddress`) with
one pointer-sized argument, so the executor's catching poll needs no closure.

std's wrappers are in `std/task/unwind.velt`: `catchVeltPanic(f): string | null`, and the raw
forms `catchPanicRaw(f)` and `catchPanicAt(code, arg)` that return the object:

```ts
if (!enterCatch()) { f(); return 0; }   // only where velt_eh_try catches
const e = unsafe { catchUnwind(f) };
leaveCatch();
if (e != 0) { caughtPanic(); caught(); }
return e;
```

**State restored after a caught panic** (`caught()`). A panic inside a transfer (a value going to
`spawn` that cannot go, a copy that failed) leaves the thread's transfer depth up, and every later
transfer of the thread would nest in the stale one. `f` runs as user code, where the depth is 0
(a transfer suspends around the user code it calls), so after a caught panic
`velt_rt_xfer_suspend()` reads the depth the panic left; if it is above 0,
`velt_rt_xfer_resume(depth)` takes the stale transfer back and `velt_rt_xfer_end()` once per level
closes it. The last `end` clears its copies and releases the references it deferred: those
belonged to the transfer, not to the abandoned frames. Test: `std/task/unwind.test.velt` "closes
a transfer the panic left open".

**The leak check.** Where the unwound frames have no cleanup pads (Windows), what they owned
leaks, so with `VELT_RC_STATS=1` `caught()` writes `rc stats: std caught a panic` to stderr, which
`velt test`'s leak check reads (`std/test/plan.velt` `leakedBlocks`) to skip the check. Elsewhere
the pads drop what the frames owned and the counts balance.

**What a catch does not undo.** Every frame between the raise and the catch is abandoned: a
`Mutex` it held stays locked, and an object it was changing keeps the change half made. A state
machine that panicked in its poll is never polled again ([Async state
machines](#async-state-machines)).

**Nesting.** The search phase stops at the nearest catching frame, so the innermost
`catchUnwind` takes the panic; an outer one sees `f` return.

### The personality

Each frame with a landing pad names `velt_eh_personality` (`velt_vir::eh::PERSONALITY`), which
the backend defines in the object. It calls only libunwind / libgcc_s functions, which every
native program links already; no Rust symbol is involved, so a debug build against the shared
runtime links too.

```text
velt_eh_personality(version, actions, class, exception, context):
  if class != VELTPANC: return _URC_CONTINUE_UNWIND          // both phases
  entry = the LSDA call-site entry covering ip - 1
  if no entry, or its landing pad is 0: return _URC_CONTINUE_UNWIND
  if entry.action == 0:                                       // a cleanup pad
    search phase: _URC_CONTINUE_UNWIND; cleanup phase: land
  else:                                                       // velt_eh_try's catch
    search phase: _URC_HANDLER_FOUND; cleanup phase: land in the handler frame only
  land: SetGR(0, exception); SetGR(1, 0); SetIP(region start + landing pad)
        return _URC_INSTALL_CONTEXT
```

The LSDA reader handles the layout both backends write: landing-pad base omitted, the type table
skipped, call sites in ULEB128 (LLVM) or `udata4` (Cranelift). A foreign class passes in both
phases, so its unwinding runs no Velt cleanup.

### The executor in Velt

`catchPanic(body)` (`std/task/panic.velt`) catches at a started promise. Its catch under the
executor in Velt is `std/task/catch_sched.velt`, the native variant of `#task-catch`; the
default variant, `std/task/catch_rt.velt`, uses the runtime's catch (`velt_rt_catch_*`) and is the
one WebAssembly builds load.

- **Arming.** `arm()` sets the low bit of the worker's `E_CATCHING` word; the next promise
  started takes it (node flag `CATCHES`, `E_GUARDED` holds it), and `take()` disarms and returns
  that promise. So the guarded promise is the first one started after `body` is called, and
  `body` should be the async call itself (`() => handle(req)`). `E_CATCHING` also counts the
  catching polls running on the worker, so a promise started while one runs catches too.
- **Polling through the catch.** The executor polls only `CATCHES` nodes through the catch
  (`caughtPoll`): its `run` tests the flag with the flags it reads anyway. The poll, state and
  context wait in the worker's argument words (`E_CATCH_ARGS`), and `catchUnwindAt` has
  `velt_eh_try` call the poll with their address. The catching poll and the raise again are
  reached through function addresses that `arm` stores in worker 0's block, so a program that
  never calls `catchPanic` has no `catchUnwind` in its live code and so no `velt_eh_try`, no panic
  hook and no cleanup pads (test `only_a_program_that_catches_has_the_catch` in
  `crates/veltc/tests/std_scheduler_catch.rs`).
- **A panicked node** keeps its exception (the message) in place of its poll and is finished; its
  result drop is cleared, and its state machine is never polled or dropped again (the pads of its
  poll dropped what it held). It is freed with its last reference like any other node, its
  exception with it: a leaf that still holds its waker holds a reference.
  Awaiting it raises the panic again in the awaiter (`raiseAgain`, which prints nothing: the
  message was printed at the panic), so a panic travels through `await`s as through calls until a
  catching poll takes it, or ends the process with 101 where none runs.
- **Waiting for the guarded promise.** `catchPanic` waits with `settled`, through a settled-future
  header inside the node, which does not claim the result or raise; when the promise panicked it
  throws `PanicError(message)`. A guarded promise that finished without a panic before
  `catchPanic` waits for it is not waited for: `take` hands back 0 and `catchPanic` takes its
  result.
- **Tasks spawned while a catching poll runs catch too.** Their root is polled through the catch
  (task flag `CATCHES`); a panic makes the task done with no result (`J_PANICKED`; nothing
  transferred, copied or dropped), and its join handle's poll becomes `joinPanicked`, which
  raises the panic again in the joiner. A panicked task nobody joins ends there and the process
  goes on, so an HTTP handler that spawns work keeps serving. Tasks that do not catch pay nothing:
  the task's poll tests `CATCHES` in a mask it tests already. A join polled again through the
  header from before the panic is woken once to poll through the new one; a second such poll ends
  the process with 101, since that `@export` frame cannot unwind to a catch.
- **Where generated code does not catch** (`velt_eh_catches() == 0`: Windows on arm64, the
  `velt dev` JIT), `caughtPoll` is a plain poll and a panic under `catchPanic` ends the process
  with 101, as an uncaught one does. There is no runtime catch as a fallback.

### Cleanup in unwound frames

A frame that owns values needing a drop gets cleanup code that runs when a Velt panic unwinds
through it, only in a program whose live code calls `catchUnwind`. Programs that do not catch are
byte-identical to programs built without the feature.

**Where cleanups come from.** Lowering already knows, at every point of a function, what the
frame owns: the scope stack of drop obligations (`crates/velt_vir/src/lower/drops.rs`: a
`DropEntry` per scope, the static state or drop flag of each local), which `return`, `break` and
`throw` drop on their edges and an async function's cancel path drops without its `finally`
blocks. A cleanup pad (`crates/velt_vir/src/lower/cleanup.rs`) is that cancel path at a call site:
the drops of every open scope, innermost first, without `finally` blocks and without `catch`
handlers (a panic is not an error: user code never sees it), then the resume of the unwind.

- **Which calls.** A call gets an unwind edge when (1) the program catches: sema demanded the
  `panic_raise` hook (the construct `CatchUnwind` is live) and the target unwinds
  (`Catching::of_triple(target) == Itanium`); (2) the callee may unwind: a Velt function or a
  function value (`velt_vir::eh::may_unwind`); externs (the runtime, C) do not get one; (3) the
  frame owns something at the call: some open scope holds an entry that would emit a drop. A
  function that never owns anything at a call is unchanged. Arguments the call takes are already
  off the scopes, so the callee drops them, never both.
- **Calls on exit edges.** `return`, `break`, `continue` and `throw` emit the drops (and `finally`
  blocks) of the scopes they leave without popping them. A call there (drop glue running a
  `[Symbol.dispose]()`, a call in a `finally`) gets a pad without what the edge has dropped or is
  dropping: each scope keeps a count of the entries pads still drop (`Scope::pad_keep`), lowered
  as the edge goes and restored after it. The old value of a reassigned local is left out of the
  pads of its own drop, and the new value, which no scope owns until it is stored, is in them.
- **A transfer that panics.** A value going to another thread is off the scopes once consumed,
  and its transfer glue can panic. `transfer_value` registers the temporary holding it for the
  glue's call and takes it back after, so the pad drops it as far as it was transferred. An
  original's release is deferred only once its copy is finished, so a panic while settling a copy
  leaves the original owned once, and the pad and `caught()` do not both release it. Tests:
  `a_value_whose_transfer_panics_is_dropped`,
  `a_copy_whose_settling_panics_releases_the_original_once`,
  `a_clone_returning_a_stored_object_releases_the_original_once`
  (`crates/veltc/tests/unwinding.rs`).
- **Pads are shared.** A pad is a chain of links, one per drop entry, innermost first, each ending
  in a jump to the pad of the entries outside it; the empty pad is one block calling
  `velt_eh_resume`. Links are cached per function by the entries and the states they read, so
  code size grows with the number of distinct ownership states at calls, not with calls ×
  entries.
- **Inside a pad** calls get no unwind edge.

`VELT_UNWIND_PADS=0` turns the edges off in lowering. It exists for measuring (the same program
with and without pads); a program built with it leaks what unwound frames own.

#### VIR

- `Terminator::Call` has `unwind: Option<BlockId>` (`vir.rs` invariant 14). `Some(pad)`: when the
  callee unwinds with a Velt exception, control continues at `pad` with `dest` not written. Every
  call of a program that does not catch has `None`.
- `velt_eh_resume()` (`velt_vir::eh::RESUME`, noreturn, backend-defined) continues the unwind that
  entered the pad being run. Each pad ends in a call of it; a call of it never has an unwind edge.
- Passes see the unwind edge as a successor (`velt_opt::visit::successors`, lowering's
  `cfg::successors`, the verifier's dataflow), so a value a pad reads is live at the call and
  definitely assigned before it. The inliner wires an inlined body into the call's edge: the
  callee's calls that may unwind and have no edge get the call's, and its `velt_eh_resume` calls
  become jumps to the call's pad.
- **Memory kept in locals across calls.** A pass that keeps a pointee's fields in locals and
  stores them back later must also store them where an unwind leaves the frame, since a frame
  further out may own that memory and drops it by what memory says. `noalias` promotion stores the
  written fields on each unwind edge, and `frame_slots` stores a poll function's written slots
  before `velt_eh_resume` as before a `return`. Test:
  `promoted_fields_are_current_when_a_call_unwinds`.

#### A panic during a cleanup

A drop in a pad can run user code (`[Symbol.dispose]()`), which may panic. Its unwind would run
the outer pads of the frame whose pad is running and drop again what the inner drop was in the
middle of. As in Rust ("panic while panicking"), it aborts: `panicRaise` sets the thread-local
`UNWINDING` just before it raises, `caughtPanic` clears it when the catch has the exception, and a
`panicRaise` that finds it set prints `panic while unwinding: aborting` and calls `abort()`.
Nothing is added to the normal path. A consequence is that a catch inside a destructor run by a
cleanup cannot catch either (test `a_panic_during_a_cleanup_aborts`).

#### Async state machines

A poll function's frame holds its live state in the state struct. Its pads are built before the
spill pass chooses what goes into the state, so a pad drops what the open scopes own wherever it
lives: the frame's temporaries and the values the state holds across suspensions alike. Every
call that may unwind gets a pad, also where the frame owns nothing, and the pads set the state's
tag to finished (`DONE`) before the resume. The frame holding the state then drops it as a
finished one, which does nothing.

A poll that unwinds through a call without a pad (an extern's) runs none of this, and the tag of
its last suspension would name values the poll may have moved since. So in a program that
catches, the start and every resume of a poll function first set the tag to `POLLING`, which has
no dispatch case: polling or dropping such a state does nothing, and what it held leaks. That is
one store per poll, only in programs that catch.

The child an `await` waits for is owned by no scope: the cancel block of its suspension drops it.
So the pad of a call that polls it drops it first, with the same code (`cleanup.rs` `awaiting`),
then what the scopes own.

A generator's resume function (and an async generator's poll function) is built the same way:
its pads drop what its state holds and mark it finished, so the frame holding the generator drops
it as a finished one, and a later `next()` is done, as after a generator that threw (test
`a_panic_in_a_generator_body_is_caught`).

## Compilation

### LLVM

`crates/velt_codegen_llvm/src/eh.rs` defines `velt_eh_try` and the personality in every module of
a program that calls it, `linkonce_odr hidden`, so the link keeps one:

```llvm
define linkonce_odr hidden ptr @velt_eh_try(ptr %code, ptr %env) noinline personality ptr @velt_eh_personality {
  invoke void %code(ptr %env) to label %ok unwind label %pad
ok:  ret ptr null
pad: %lp = landingpad { ptr, i32 } catch ptr null
     %e = extractvalue { ptr, i32 } %lp, 0
     ret ptr %e
}
```

`uwtable` is on every compiled function. A call with an unwind edge is an
`invoke ... to label %ok unwind label %lpN`; each pad reached by an unwind edge gets a landing
block, `landingpad { ptr, i32 } cleanup`, which stores the exception pointer in the function's
exception slot and branches to the VIR pad. `velt_eh_resume` is `resume` of the slot. LLVM's
inliner prices the pads as if they ran on every call, so a function that is small outside its
pads (`inlining::small_with_pads`) gets `inlinehint`, and velt_opt's single-call-site rule
compares its cost limit with the code outside the pads (`inline::hot_cost`). Both apply only to
functions with unwind edges.

### Cranelift

`crates/velt_codegen_cl/src/eh.rs`: `velt_eh_try` is a `try_call_indirect` with one catch-all
handler (`ExceptionTableItem::Default`) whose block takes the first exception payload register. A
call with an unwind edge is a `try_call` whose handler is a landing block per pad that stores the
exception in a variable and jumps to the pad; `velt_eh_resume` calls `_Unwind_Resume` with it.

`try_call` exists only for the SystemV convention. On Linux and x86-64 macOS that is the
platform's; on Apple arm64 a call uses a SystemV signature for the same symbol only when every
argument is in a register (at most 8 integer and 8 float or vector arguments), where the two
conventions agree; a call with stack arguments stays a plain call. After a function is compiled,
its call sites with a handler become its LSDA (`.gcc_except_table` / `__TEXT,__gcc_except_tab`,
call sites in `udata4`), and its FDE names the LSDA and a CIE with the personality, which the
object defines once when any function has an LSDA. The personality pointer is indirect: on ELF
through a local pointer in `.data` (`DW.ref.*`), on Mach-O through the GOT
(`ARM64_RELOC_POINTER_TO_GOT`, `X86_64_RELOC_GOT`); ld64 accepts no other personality
relocation. A `try_call` treats every register as clobbered on the exceptional edge, so values
live across it are spilled: a debug-build cost only.

The JIT (`velt dev`, generator hosts) registers no LSDA, so `velt_eh_try` there is a plain call
that returns null and no call has a landing pad.

### A function C calls

A function exported to C (`@export`, `Function::stops_unwind`) stops a Velt panic: an unwind that
reaches it ends the process. It gets no cleanups. With LLVM its calls are `invoke`s of an abort pad
(`crates/velt_codegen_llvm/src/function/stop_unwind.rs`); with Cranelift it has no unwind entry,
so the search ends at it, the raise returns and `panicRaise` exits with 101. Test:
`a_panic_does_not_unwind_through_c_frames`.

### Windows

The unwinder is SEH. Rust panics on MSVC are C++ exceptions, and a Rust `catch_unwind` there
matches only its own type descriptor, so foreign exceptions pass Rust frames. Windows x64 catches
(`velt_vir::eh::Catching::Seh`); Windows on arm64 does not (`Catching::None`).

- **Raise** (`std/lang/hooks/unwind_seh.velt`): the object as on Itanium, with its own address at
  offset 8, raised with `RaiseException(0xE0564C54, EXCEPTION_NONCONTINUABLE, 1, object + 8)`
  (`velt_vir::eh::SEH_CODE`), so its one parameter is the object.
- **Catch, LLVM** (`crates/velt_codegen_llvm/src/eh.rs`): what clang emits for
  `__try/__except(filter)`: `velt_eh_try` with personality `__C_specific_handler`, the call an
  `invoke` to a `catchswitch` with one `catchpad` naming a filter. The filter returns
  `EXCEPTION_EXECUTE_HANDLER` for `0xE0564C54` (after storing the parameter in a slot of
  `velt_eh_try`'s frame, `llvm.localescape` / `llvm.localrecover`) and `EXCEPTION_CONTINUE_SEARCH`
  otherwise.
- **Catch, Cranelift** (`crates/velt_codegen_cl/src/eh_seh.rs`): `try_call` has no Windows
  convention, so `velt_eh_try` is fixed machine code, the filter included, whose `.xdata` names
  `__C_specific_handler` and a one-entry scope table.
- **No cleanup pads.** Lowering adds unwind edges only for `Catching::Itanium`, so the frames a
  panic unwinds on Windows leak what they own.
- **A function C calls** stops a Velt panic: with LLVM its abort pad is a `cleanuppad` under
  `__C_specific_handler`; with Cranelift its `.xdata` names a language handler (`velt_eh_stop`)
  that ends the process with 101 when the search phase reaches it.
- **An unclaimed raise** never returns on Windows, so the first raise installs a filter of its own
  (`SetUnhandledExceptionFilter`, `unclaimed` in `unwind_seh.velt`) that ends the process with 101
  for Velt's code and hands other exceptions to the filter it replaced. Raises racing on several
  threads wait until it is in place (`INSTALLED`). A filter installed later replaces it, as any
  later filter does.

### WebAssembly

No unwinding: `velt_rt_wasm` aborts on a panic and `velt_eh_try` is a plain call. WebAssembly
keeps the runtime's executor and its catch (`std/task/catch_rt.velt`).

### Contracts

- `hir::Intrinsic::CatchUnwind` and `CatchUnwindAt` (`crates/velt_sema/src/hir/intrinsic.rs`),
  declared in `std/lang/intrinsics/unwind.velt`.
- velt_common: the hook `panic_raise` (`std/lang/hooks/unwind::panicRaise`, `(&string) -> never`,
  `constructs(CatchUnwind)`), the construct `Construct::CatchUnwind`, which demands only the panic
  hook, and the std module `lang_hooks_unwind`, loaded on demand.
- VIR: `Terminator::Call::unwind` (invariant 14) and the backend-defined externs
  `velt_eh_try(ptr, ptr) -> ptr`, `velt_eh_catches() -> u8` and `velt_eh_resume()`
  (`velt_vir::eh`), symbol contracts like `velt_tls_*`.
- Objects of programs that catch: `velt_eh_try`, `velt_eh_personality` (local), an LSDA per
  function with landing pads, a CIE naming the personality; references to
  `_Unwind_RaiseException`, `_Unwind_Resume`, `_Unwind_GetIP`, `_Unwind_SetGR`, `_Unwind_SetIP`,
  `_Unwind_GetRegionStart` (and, with LLVM, `_Unwind_GetLanguageSpecificData`): libSystem on
  macOS, libgcc_s on Linux, both linked already.

## Runtime cost

**Programs that do not catch** get the same code as without the feature: no `velt_eh_try`, no
landing pads, no `POLLING` stores (tests `a_program_without_catch_keeps_the_runtime_panic` and
`only_a_program_that_catches_gets_cleanup_pads`).

**A catch that catches nothing.** An `invoke` (a `try_call`) is a plain call plus a call-site
table entry; the landing pad is out of line. `velt_eh_try` is `noinline`, so a catch costs one
call and the indirect call of `f`; `catchVeltPanic` adds two thread-local updates. Measured on
Linux arm64 with cachegrind, 2,000,000 iterations of a closure that adds to a module variable:

| | release (LLVM) | per iteration | debug (Cranelift) | per iteration |
|---|---:|---:|---:|---:|
| plain call | 122,393,097 | | 350,566,152 | |
| `catchUnwind` | 144,393,079 | +11 | 346,567,849 | −2 |
| `catchVeltPanic` | 172,393,245 | +25 | 538,572,083 | +94 |

In release the plain call is inlined and the catch is not. In a debug build a `ThreadLocal` costs
a call each. The executor pays this once per poll of a catching promise.

**A caught panic** 10 frames deep, with its message printed to stderr, costs about 74,000
instructions in release and 89,000 in debug: mostly the two-phase search and the cleanup phase
through the DWARF unwinder, and the message's text. It is a cold path.

**Cleanup pads** cost code size in programs that catch: one link per distinct drop entry and
ownership state at calls, out of line. The normal path is unchanged except for the `POLLING` store
per poll.

**Size.** Per object that catches: `velt_eh_try` and the personality (a few hundred bytes of
code), an LSDA per function with landing pads, one more CIE, and an 8-byte personality pointer on
ELF.

## Diagnostics

All at run time, on stderr:

- `panic: <msg>`: printed at the panic, caught or not.
- `panic while unwinding: aborting`: a panic in a drop that a cleanup pad runs; the process
  aborts.
- An uncaught panic, or one that reaches a frame that cannot unwind to a catch, ends the process
  with exit code 101.

## Alternatives considered

**Catching the Rust panic in generated code.** A Rust panic may be ended only by Rust; a foreign
frame that deletes it makes Rust's cleanup routine abort. It would also keep the panic path in
Rust while the runtime moves into std.

**Another personality routine.** `__gxx_personality_v0` (C++) catches a foreign class only with
`catch (...)`, which must rethrow, and needs libc++abi or libstdc++, which are not linked and not
in a static musl build. `__gcc_personality_v0` runs cleanups only, never a handler.
`rust_eh_personality` claims any class whose action is a catch, without checking the class, and
the shared runtime library does not export it. Velt's own routine is small, checks the class, and
needs only the unwinder.

**Always raising, without the thread-local count.** Raising on every panic and exiting when
nobody claims it would walk the stack for every uncaught panic, and on Itanium targets a Rust
`catch_unwind` above the raise would claim and then abort on the foreign exception. The count
costs two thread-local updates per catch.

**One fixed `velt_eh_try` for both backends on Windows.** LLVM's own SEH lowering produces the
same frame clang does for `__try/__except` and needs no hand-written tables; Cranelift, which has
no Windows `try_call`, uses fixed machine code.

**A request-scoped arena instead of cleanup pads.** Freeing a request's allocations in one step
would avoid per-frame cleanup code, but it changes allocation for every program and does not drop
values that outlive the request's frames. Cleanup pads reuse the drops lowering already computes
and cost nothing in programs that do not catch.

## Limitations

- **Not covered by cleanup pads** (what they own leaks): calls of externs; the shared
  `division by zero` panic blocks; Cranelift on Apple arm64 for a call with stack arguments; the
  JIT; Windows; WebAssembly. `velt test` fails the leak check of a test that catches a panic
  unwinding through one of these on a target with pads.
- **A panicked task's root** is never freed
  ([#2818](https://github.com/velt-lang/velt/issues/2818)).
- **`catchPanic` guards the first promise started after `body` is called**, so `body` must be the
  async call itself.
- **A foreign exception that passes a catch leaves the count up.** One that passes
  `catchVeltPanic` and is caught above it skips `leaveCatch()`. This cannot happen: the only way a
  foreign frame calls Velt code is an `@export` function, which stops every exception that
  unwinds out of it.
- **A catch inside a transfer.** `caught()` closes every open transfer level, which assumes `f`
  started at depth 0; a `catchVeltPanic` run from a class's `clone()` during a transfer would
  close the outer transfer too. A transfer the panicked code had suspended stays on the runtime's
  stack of suspended transfers.
- **Runtime function pointers that are still `extern "C"`.** A Velt exception through one of them
  aborts instead of reaching the catch: `DropFn` in `crates/velt_rt/src/drop_depth.rs` and
  `crates/velt_rt/src/transfer_map.rs`; `DataFn`, `TransferFn` and `DropFn` in
  `crates/velt_rt/src/fn_props.rs`; `WorkFn` and `DropCtxFn` in `crates/velt_rt/src/native/mod.rs`.
- **The payload is the message only.** No location and no value.

## Not yet implemented

- **The JIT.** `velt dev` registers the LSDA and the personality's CIE for JIT code (the same bytes
  `__register_frame` takes in `crates/velt_codegen_cl/src/unwind/jit_systemv.rs`), so a panic in a
  handler under `velt dev` answers 500 instead of ending the process.
- **Windows on arm64**, with SEH as on x64, and cleanup pads on Windows.
- **`C-unwind` for the runtime's function pointers** listed under Limitations, each with its own
  measurement.
- **The payload** becomes `{ message, location }`, the message string moved in rather than copied
  as NUL-terminated bytes (with `addressStore`).
- **A panic a drop catches itself.** As in current Rust, a panic during a cleanup that a catch
  inside the drop takes does not abort; only one that escapes the drop does. `UNWINDING` becomes a
  count of the catches entered since the unwind began.
- **`catchPanic` marks the node it starts itself**, replacing the arming of the next started
  promise, once std can get a promise's address before its start.
- **The runtime's catch is deleted** once WebAssembly no longer uses it: `velt_rt_catch_*`
  (`rt_abi_async.md` §1.5), `velt_rt::panic::catch`, `task/local/catch.rs`, `VeltPanic`, and
  `velt_rt_panic` reduced to print and exit 101, which can then move to std.
