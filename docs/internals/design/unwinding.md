# Design: Velt's own unwinding, and catching a panic in generated code

Status: draft (#2649 decision 3, follows #2008 / #2137; tracking #2196). A prototype of the core
is on branch `velt-unwind`: [Prototype](#prototype). The owner's open questions are in
[Questions](#questions-for-the-owner).

## Problem

Since #2137 a panic is a real unwind. `velt_rt_panic` (Rust, `extern "C-unwind"`) ends the
process unless a promise up the stack catches panics; then it unwinds with a Rust panic payload
(`VeltPanic`) through the compiled frames, which have unwind tables and no landing pads, to a
Rust `catch_unwind` at the poll of the catching promise node (`velt_rt::panic::catch`,
`task/local/catch.rs`). `catchPanic` in `velt/task` arms that catch through
`velt_rt_catch_arm/take/settled/message/free`.

The executor in Velt (#2649) polls tasks from Velt code, so there is no Rust frame left to
catch in. Owner decision 3: no temporary runtime catch under the Velt executor; solve unwinding
first, and build the started promise's catch on it. Generated code cannot simply catch today's
panic object: a Rust panic is an exception of class `MOZ\0RUST`, and only Rust may end one (its
cleanup routine aborts when a foreign frame deletes it: "Rust panics must be rethrown"). And
the new code must not be Rust: "nothing new in the runtime" (#2196).

## Summary

- **Velt raises its own exception.** Class `VELTPANC` (`velt_vir::eh::CLASS`), raised by std
  code with the platform's unwinder: `_Unwind_RaiseException` on macOS and Linux (and every
  other Itanium-ABI target), `RaiseException` on Windows. No Rust is involved.
- **Generated code catches it.** A std-only intrinsic, `catchUnwind(f)`, calls `f` at a call
  site with a landing pad (LLVM `invoke`, Cranelift `try_call`) and returns the exception object
  when a Velt exception unwound out of `f`, 0 when `f` returned.
- **Only Velt's class is caught.** The landing pad's personality claims `VELTPANC` in the search
  phase and nothing else: a C++ exception, a Rust panic, any foreign one passes as if the frame
  had no landing pad.
- **Unwound frames drop what they own**, in programs that catch (#2667): calls in frames that own
  values get cleanup pads that drop them and resume the unwind
  ([Cleanup in unwound frames](#cleanup-in-unwound-frames)). A panicked state machine is never
  polled or dropped again; its frame's own temporaries are dropped.
- **The executor in Velt catches at a started promise with it**, which deletes the runtime's
  catch: `velt_rt_catch_*`, `panic::catch`, `Catching`, `MAY_CATCH`, the Rust payload and the
  node's panicked bookkeeping in Rust.
- **Panics of Rust code stay fatal.** A Rust panic in velt_rt (a bug, an ICE) still ends the
  process from the panic hook before any unwinding; nothing catches it.
- **WebAssembly does not unwind.** A panic ends the instance as today; `catchUnwind` is a plain
  call that returns 0.

## What is raised, and by whom

The raise is std code (`std/lang/hooks/unwind.velt`, `panicRaise`), reached from every panic
site of a program that catches: lowering calls the hook where it called `velt_rt_panic`
(`Rt::Panic`: `panic(msg)`, index and key checks, `unreachable`, transfer and copy failures).

```text
panicRaise(msg):
  if this thread runs a catchUnwind that catches (thread-local count > 0):
    flush stdout; print "panic: <msg>" to stderr        // as today, at the panic
    e = malloc(32 + len + 1); header: class VELTPANC, no cleanup, two zero words
    copy msg after the header; _Unwind_RaiseException(e)
    // a live velt_eh_try claims it, so this returns only in the hole below (or if malloc
    // failed): exit 101, as an uncaught panic
  velt_rt_panic(msg)                                     // today's path, unchanged
```

**Per target.** The raise is chosen by a conditional import of std (`#unwind-raise`,
std/imports.velt): `std/lang/hooks/unwind_itanium.velt` on native targets other than Windows
(the Itanium unwinder), `unwind_none.velt` on Windows (`win32`) and WebAssembly, which raises
nothing and names no unwinder function, so those programs link (Windows CI failed with
`_Unwind_RaiseException` unresolved before). The backends name no `_Unwind_*` function there
either: `velt_eh_try` is a plain call without a personality (`Catching::None`).

**What the count means.** `CATCHING > 0` means a `velt_eh_try` frame *with a landing pad* is live
on this thread's stack. `enterCatch` asks the backend first: `velt_eh_catches()` (a constant
function the backend defines next to `velt_eh_try`, `velt_vir::eh::CATCHES`) is 1 in objects for
the Itanium targets and 0 in the JIT (`velt dev`, generator hosts: no LSDA registered yet),
on Windows and on WebAssembly. Where it is 0, `catchVeltPanic` calls `f` plainly and doesn't
count, so a panic takes `velt_rt_panic` (exit 101, or the runtime's `catchPanic`) instead of
raising an exception nothing would claim. So the innermost live `velt_eh_try` claims every raise,
except in the hole below.

**Why the thread-local count.** On Itanium targets a Rust `catch_unwind` claims *every*
exception in the search phase (Rust's personality doesn't look at the class) and then aborts
on a foreign one ("Rust cannot catch foreign exceptions"). Under the runtime's executor such
frames are on most stacks (tokio's task harness, `task/runtime/velt/task.rs`; the runtime's own
catch). So a Velt exception is raised only when a Velt catch is the nearest catching frame,
which the count says: `catchUnwind` wraps synchronous code, so between it and the raise there
are only compiled frames and Rust `C-unwind` frames without a catch. Without a catch the panic
takes today's path (exit 101, or the runtime's `catchPanic` while it exists). The count also
keeps the uncaught panic from walking the stack twice. Under the Velt executor both Rust catch
sites are deleted; the count stays, for the second reason.

**The documented hole** while both executors exist: an `f` that itself makes the runtime poll a
catching promise (the runtime's `catchPanic` inside `catchUnwind`) puts a Rust `catch_unwind`
between the raise and the catch. Rust's personality claims the Velt exception in the search
phase and its catch then aborts the process ("Rust cannot catch foreign exceptions"), with a
runtime message instead of exit 101. Only the executor's own catch uses `catchUnwind`, so the
hole closes with the switch, when the runtime's catch is deleted.

**What a raise nobody claims does.** `_Unwind_RaiseException` returns only when the search phase
reaches the end of the stack without a frame claiming the exception. Whether it gets there
depends on the frames above `main`, which Velt doesn't control: the runtime's entry functions
(`main`, `velt_rt_start`, the `program` thunk) are Rust `extern "C"` functions, whose
abort-on-unwind landing pads Rust's personality claims for any class, so on Linux x86-64 with a
debug runtime the process aborts there ("panic in a function that cannot unwind"); with other
runtime builds the search reached the end of the stack and the raise returned. Tokio's task
harness on worker threads catches too. So `panicRaise`'s exit 101 after a returned raise is a
fallback, not a guarantee: the guarantee is that it raises only when a `velt_eh_try` that
catches is live, which then claims it.

**The exception object.** One `malloc`:

| offset | size | field |
|---:|---:|---|
| 0 | 8 | `exception_class` = `VELTPANC` (little-endian bytes) |
| 8 | 8 | `exception_cleanup` = null (the unwinder never deletes it; the catcher frees it) |
| 16 | 16 | the unwinder's private words (zeroed) |
| 32 | n+1 | the message, UTF-8, NUL-terminated |

The prototype writes the message as bytes because std has no raw store yet; with
`addressStore` (#2196, branch `velt-sched-intrinsics`) the payload becomes `{ VeltStr message,
u64 flags }`, the string moved in (no copy, no NUL limit), and later the panic's location and a
payload value if `catchPanic` ever hands one over. The object never leaves its thread: a catch
and its raise are on one stack. `takePanic(e)` reads the message and frees the object.

**Rust code inside a catch.** Rust frames between the raise and the catch (a runtime leaf that
polls a compiled future, the drop glue it calls) are `extern "C-unwind"` since #2137, so the
foreign exception passes them and their own cleanups (drops of Rust locals) run, which is
correct for a foreign exception. A Rust `extern "C"` function on that path would abort the
process ("panic in a function that cannot unwind"); the rule of #2137 stays: every runtime
function that can run Velt code is `C-unwind`.

**Panics of Rust code.** The runtime's panic hook (`install_hook`) ends the process before a
Rust panic unwinds, with `panic: <msg> (runtime: file:line)`. Unchanged: they are bugs, and
catching them would hide that the runtime's state is broken. A user error the runtime detects
(`panic::raise` for an awaited panicked node, `fatal`) keeps ending the process; under the
Velt executor the awaited-panicked case moves to std (below), and the rest become errors
returned to std, which panics through `panicRaise`.

## The catch: `catchUnwind`

```ts
@intrinsic
export declare unsafe function catchUnwind(f: () => void): usize;
```

`std/lang/intrinsics/unwind.velt`, `hir::Intrinsic::CatchUnwind`. Lowering calls
`velt_eh_try(f.code, f.env)`, a function the backend defines in the object (`velt_vir::eh`;
like `velt_tls_*`, no runtime function is behind the symbol). It returns null when `code(env)`
returned and the exception object when a Velt exception unwound out of it. The caller owns the
object. std's wrapper is `catchVeltPanic(f): string | null` (`std/task/unwind.velt`):

```ts
if (!enterCatch()) { f(); return null; }   // the count, only where velt_eh_try catches
const e = unsafe { catchUnwind(f) };
leaveCatch();
if (e == 0) return null;
caught();                                  // transfer state, leak check: below
return unsafe { takePanic(e) };
```

**State the runtime's catch restores.** `velt_rt::panic::catch` calls
`transfer_map::forget_since(open)`: a panic inside a transfer (a value going to `spawn` that
can't go, a copy that failed) leaves the thread's transfer depth up, and every later transfer of
the thread would nest in the stale one (its copy map never cleared, its deferred releases never
run). `catchVeltPanic` restores it with the existing externs, on the catch path only: `f` runs as
user code, where the depth is 0 (a transfer suspends around the user code it calls), so after a
caught panic `velt_rt_xfer_suspend()` reads the depth the panic left; if it is above 0,
`velt_rt_xfer_resume(depth)` takes the stale transfer back and `velt_rt_xfer_end()` once per
level closes it. The last `end` clears its copies and releases the references it deferred: those
belonged to the transfer (sources it replaced by copies), not to the abandoned frames, so they
are released rather than leaked (the runtime's `forget_since` leaks them). Not restored: a
transfer the panicked code had *suspended* (a panic inside a class's own `clone()` during a
transfer inside `f`) stays on the runtime's stack of suspended transfers. With nothing suspended
when `f` started that is a leaked entry and nothing else; under a suspended outer transfer
(`catchVeltPanic` inside a `clone()` that runs during a transfer, then that nested panic), the
outer `resume` would take the stale entry. No extern reads that stack's length; the transfer map
moves to std with the executor (#2196), and then the catch records and truncates it as
`forget_since` does. Limit (review of #2668): `caught()` closes every open transfer level, which
assumes `f` started at depth 0; a `catchVeltPanic` that runs inside a transfer (from a class's
`clone()` during one) would close the outer transfer too. The catch should record the depth on
entry and close only the levels above it, which needs that depth read without suspending (no
extern does that today; std's transfer map will). Test: `std/task/unwind.test.velt` "closes a transfer the panic left open"
(#2668's repro: `spawn(run(mk(keep)))` with a captured weak collection; the depth is 0 after the
catch and a later spawn copies a graph with its sharing).

**The leak check.** A caught panic leaks what the abandoned frames held, so a leak check of the
process can't pass; the runtime's catch counts itself (`panics caught=` in the `VELT_RC_STATS`
report) and `velt test` skips the check then. std's catch has no runtime counter (no new
extern), so with `VELT_RC_STATS=1` it writes `rc stats: std caught a panic` to stderr, which the
leak check reads too (`std/test/plan.velt` `leakedBlocks`). With the executor's switch the
counter becomes std's altogether.

**What is leaked.** Since #2667 the owned values of the frames between the raise and the catch
are dropped by their cleanup pads ([Cleanup in unwound frames](#cleanup-in-unwound-frames), with
its list of what is not covered). The rest of this paragraph is what stays true: every frame
between the raise and the catch is abandoned: a `Mutex` they held stays locked,
an object they were changing keeps the change half made (the README of `catchPanic` says so
today). A state machine that panicked in its poll is never polled or dropped again, since its
fields may be half moved; its task header stays allocated, so a waker that still points at it
wakes nothing harmful. That is the contract of #2008 and doesn't change.

**Nesting.** The search phase stops at the nearest catching frame, so the innermost
`catchUnwind` takes the panic; an outer one sees `f` return.

**The cost of a catch that catches nothing.** An `invoke` (a `try_call`) is a plain call plus a
call-site table entry; the landing pad is out of line. `velt_eh_try` is a function of its own
(`noinline`, so its personality needs no call-site lookup), so a catch costs one call and the
indirect call of `f` that a plain closure call would make anyway. The wrapper adds two
thread-local updates. Numbers: [Measurements](#measurements).

## The personality

Each frame with a landing pad names a personality routine in its CIE. Only `velt_eh_try`'s frame
names `velt_eh_personality`, which the backend defines next to it:

```text
velt_eh_personality(version, actions, class, exception, context):
  if class != VELTPANC: return _URC_CONTINUE_UNWIND     // search and cleanup phase alike
  if actions & _UA_SEARCH_PHASE: return _URC_HANDLER_FOUND
  if actions & _UA_HANDLER_FRAME:                         // the frame the search chose
    _Unwind_SetGR(context, 0, exception); _Unwind_SetGR(context, 1, 0)
    _Unwind_SetIP(context, _Unwind_GetRegionStart(context) + landing_pad)
    return _URC_INSTALL_CONTEXT
  return _URC_CONTINUE_UNWIND
```

Returning `_URC_CONTINUE_UNWIND` for a foreign class in the search phase is what makes a foreign
exception pass: the frame doesn't claim it, and since compiled frames have no cleanups, skipping
the frame in the cleanup phase loses nothing.

**A foreign exception leaves the count up.** One that passes `catchVeltPanic` and is caught above
it (a C++ `catch` around a Velt call) skips `leaveCatch()`, so `CATCHING` stays 1 too high on
that thread: later panics there raise with no live catch, which is the abort above. It can't
happen today: the only way a foreign frame calls Velt code is an `@export` function (#2662), and
its landing pad aborts on any exception that unwinds out of it, whatever the class (LLVM:
`function/stop_unwind.rs`, Rust's personality runs the pad in the cleanup phase; Cranelift: no
unwind entry, so the search ends there), so a foreign exception never reaches
a catcher above a Velt frame. The fix, if exports ever let exceptions through: the personality
also lands at the pad for a foreign class in the cleanup phase (selector 1, never in the search phase, so the exception still passes),
`velt_eh_try` returns the object tagged (bit 0), and `catchVeltPanic` calls `leaveCatch()` and
then `_Unwind_Resume` on it. It isn't built in this change because nothing can test it: the
phase-2 pass needs a foreign catcher above a Velt frame.

**No call-site lookup, no runtime.** `velt_eh_try`'s only call is the one with the landing pad,
so the personality needs no general LSDA reader. Cranelift knows the handler's offset when it
writes the personality and puts it in as a constant. LLVM writes the LSDA itself, so its
personality reads the first (only) call-site entry: `velt_eh_try` is `noinline`, and LLVM writes
the call-site table in ULEB128 with no landing-pad base. It checks that layout (lpstart
omitted, call-site encoding `0x01`, a nonzero landing pad) in the search phase already; an LSDA
of another layout makes the frame claim nothing (`_URC_CONTINUE_UNWIND`), rather than landing at
a wrong address or failing phase 2 at its handler frame. The routine calls only libunwind /
libgcc_s functions, which every native program links already. No Rust symbol is involved, so a
debug build against the shared runtime links too: an early version delegated to
`rust_eh_personality`, which `libvelt_rt_shared` doesn't export.

The alternatives, for the record:

| personality | catches a foreign class? | available? |
|---|---|---|
| `__gxx_personality_v0` (C++) | only with `catch (...)`, then we must rethrow | needs libc++abi / libstdc++: not linked, not in a static musl build |
| `__gcc_personality_v0` (C cleanups) | no: cleanups only, never a handler | libgcc / compiler-rt |
| `rust_eh_personality` | yes: it claims any class whose action is a catch, without checking the class | inside velt_rt's static library; not exported by the shared one; goes away with runtime elimination |
| a general one in Velt | as written | needs `@export` with the C convention and `fnAddress` (#2203 step 5) |

A general personality (any number of call sites, so that `velt_eh_try` could be inlined) is
worth writing only if inlining the helper measures worth it; then it is std Velt code (about 60
lines: the LSDA header, the call-site search, the same three unwinder calls).

## Lowering

**LLVM** (`velt_codegen_llvm/src/eh.rs`), in every module of a program that calls it:

```llvm
define linkonce_odr hidden ptr @velt_eh_try(ptr %code, ptr %env) noinline #0 personality ptr @velt_eh_personality {
  invoke void %code(ptr %env) to label %ok unwind label %pad
ok:  ret ptr null
pad: %lp = landingpad { ptr, i32 } catch ptr null
     %e = extractvalue { ptr, i32 } %lp, 0
     ret ptr %e
}
```

plus the personality above, with a ULEB128 reader. `uwtable` is already on every compiled
function (#2137).

**Cranelift** (`velt_codegen_cl/src/eh.rs`; cranelift 0.132 has `try_call`): `velt_eh_try` is a
`try_call_indirect` with one catch-all handler (`ExceptionTableItem::Default`) whose block takes
the first exception payload register (x0 / rax). The callee signature uses `CallConv::SystemV`
even on Apple arm64: Cranelift implements `try_call` only for SystemV (and Tail), and for one
pointer argument and no result the two conventions agree. Cranelift records the handler's
offset (`MachBuffer::call_sites`); the backend writes:

- the LSDA, in `.gcc_except_table` / `__TEXT,__gcc_except_tab`: no landing-pad base, a type table
  of one `udata4` entry (0: catch-all), one ULEB128 call-site entry covering the whole function
  (its only call is the `try_call`) with the handler as landing pad and action 1;
- a second CIE with the personality and the LSDA encoding, used by `velt_eh_try`'s FDE only.
  The personality pointer is indirect: on ELF through a local pointer in `.data` (compilers'
  `DW.ref.*`), on Mach-O through the GOT (`ARM64_RELOC_POINTER_TO_GOT`, `X86_64_RELOC_GOT`). ld64
  reads no other personality relocation: a pc-relative one to a data word fails the link with
  "r_symbolnum out of range" (found while prototyping).

The JIT (`velt dev`, generator hosts) gets `velt_eh_try` as a plain call for now: registering
an FDE with a personality for JIT code is `jit_systemv.rs`'s `__register_frame` plus the same
LSDA in JIT memory. That is needed before `velt dev` runs an HTTP server under the Velt executor
(its handlers' catch).

**Windows.** The unwinder is SEH, not Itanium. Rust panics on MSVC are C++ exceptions
(`0xE06D7363`, `_CxxThrowException`), and a Rust `catch_unwind` there matches only its own type
descriptor, so foreign exceptions pass Rust frames (unlike on Itanium).

- Raise: `RaiseException(0xE0564C54 /* 'VLT' */, EXCEPTION_NONCONTINUABLE, 1, &object)` from std
  (`declare c("kernel32")`).
- Catch, LLVM: `velt_eh_try` with personality `__C_specific_handler` and a filter function
  (`catchswitch` / `catchpad within %cs [ptr @velt_eh_filter]`, what clang emits for
  `__try/__except`): the filter returns `EXCEPTION_EXECUTE_HANDLER` for `0xE0564C54` and
  `EXCEPTION_CONTINUE_SEARCH` otherwise.
- Catch, Cranelift: `try_call` has no Windows convention (`exception_payload_types` is empty for
  `WindowsFastcall`). The backend writes `velt_eh_try` as fixed machine code for x86_64 and arm64
  with its `.xdata` naming `__C_specific_handler` and a one-entry scope table (the call's range,
  the filter, the jump target). It already writes x64 `.xdata` itself (`unwind/winx64.rs`). The
  same bytes could serve LLVM builds.
- To verify on Windows before the switch: that `__CxxFrameHandler3` runs Rust's cleanup funclets
  for a non-C++ exception passing a `C-unwind` frame.

The prototype defines `velt_eh_try` without a catch on Windows (`Catching::None`). That is the
part left before all three OSes can switch together (decision 2 of #2649).

**WebAssembly.** No unwinding (`velt_rt_wasm` aborts on a panic). `velt_eh_try` is a plain call.
The exception-handling proposal (`exnref`, `try_table`) could later carry the same design.

## The executor in Velt

What replaces the runtime's catch (`velt_rt_catch_*`, the catch at a started promise's poll):

- **A promise node that catches** (`catchPanic`'s body, an HTTP handler's) has a flag in its
  header. The executor polls such a node through the catch:

  ```ts
  let r: u32 = PENDING;
  const e = unsafe { catchUnwind(() => { r = futPoll(node.fut, cx); }) };
  ```

  The closure lives on the stack (it doesn't escape); its cost is one indirect call more than
  a plain poll, paid only by catching nodes, as `MAY_CATCH` arranges today. A direct form,
  `catchPoll(f, cx)` lowered to a `velt_eh_try_poll(fut, cx)` that calls the header's poll
  through the catch, saves the closure if it measures worth it.
- **When the poll panicked**, the node becomes *panicked*: finished, its result slot never read,
  transferred or dropped, its state machine never polled or dropped, the node never freed (a
  leaf inside the leaked state may still wake it). The message moves into the node.
- **Awaiting a panicked node raises the panic again** in the awaiter (`panicRaise(message)`), so
  a panic travels through `await`s as through calls, until a catching node takes it, or reaches
  a poll that catches nothing, which ends the process with 101 as today.
- **`catchPanic(body)`** starts `body()`'s promise as a catching node and awaits it without
  claiming its result; when it panicked it throws `PanicError(message)`. The arm/take protocol
  and its limits ("the first promise started after `body` is called") go: the std function
  marks the node it starts itself.
- **A spawned task** whose root panics ends the process, as today; an `@export` function a C
  caller runs aborts at its boundary on a Velt exception (decision (b) of #2649: its frame gets a
  catch-all landing pad that calls `abort`, or `nounwind`).

Deleted from velt_rt when the executor switches: `velt_rt_catch_arm`, `_take`, `_settled`,
`_message`, `_free` (rt_abi_async.md §1.5); `panic::{catch, Catching, CATCHING, MAY_CATCH,
VeltPanic, unwind, raise}`; `task/local/catch.rs`, `node/panicked.rs`; the `count_caught` leak
report becomes std's. `velt_rt_panic` keeps only "print, exit 101" and can move to std.

## Contract changes

The prototype's, all additive:

- `hir::Intrinsic::CatchUnwind` (velt_sema `hir/intrinsic.rs`) and its declaration in
  `std/lang/intrinsics/unwind.velt`.
- velt_common: hook `panic_raise` (`std/lang/hooks/unwind::panicRaise`, `(&string) -> never`,
  `constructs(CatchUnwind)`), construct `Construct::CatchUnwind` (its own bit in sema's
  `demanded_for`, so it demands only the panic hook), std module `lang_hooks_unwind`, loaded on
  demand (`ON_DEMAND_MODULES`). With laziness off (`VELT_EAGER_STD=1`) sema demands every hook
  but this one, which keeps the VIR of every program the same both ways (`eager_std_identity`);
  a program that catches then panics through the runtime.
- VIR: the backend-defined externs `velt_eh_try(ptr, ptr) -> ptr` and `velt_eh_catches() -> u8`
  (`velt_vir::eh`), symbol contracts like `velt_tls_*`; no type changes. std declares
  `velt_eh_catches` as an ordinary extern; the backend defines it.
- std's conditional import `#unwind-raise` (std/imports.velt).
- Objects: `velt_eh_try`, `velt_eh_personality` (local; LLVM `linkonce_odr hidden`), an LSDA, a
  second CIE; programs that catch call `_Unwind_RaiseException`, `_Unwind_SetGR`,
  `_Unwind_SetIP`, `_Unwind_GetRegionStart` (and, with LLVM, `_Unwind_GetLanguageSpecificData`):
  libSystem on macOS, libgcc_s on Linux, both linked already.

Later, with the executor's switch:

- `rt_abi_async.md` §1.5 deleted; `rt_abi.md` "Control": `velt_rt_panic` no longer unwinds (it is
  `extern "C"` again or std's), and a Velt exception (class `VELTPANC`) may pass any
  `C-unwind` runtime function; the runtime never `catch_unwind`s around code that runs Velt
  code.
- The started-promise node header gains the catching / panicked flags (the executor's own
  layout, std).
- Windows: the SEH raise and catch above; the JIT: LSDA registration.

## Prototype

Branch `velt-unwind`: the intrinsic, both backends' `velt_eh_try` and personality, the std panic
path and `catchVeltPanic`. Tested on macOS arm64 with Cranelift (debug) and LLVM (release), and
on Linux arm64 in the velt-prof VM ([Measurements](#measurements)):

- `std/task/unwind.test.velt`: null when nothing panics; `panic("boom")` caught with its
  message; an index out of bounds 20 calls deep caught, the code after it in the body not run,
  the process going on; nested catches (the inner one takes it).
- `crates/veltc/tests/unwinding.rs`, both backends: the same, plus an exception of another class
  (`OTHRLANG`, raised with `_Unwind_RaiseException`) passing the catch (the catch returns null
  and the raise returns: nobody claimed it), and an uncaught panic still ending with 101. A
  personality that claims every class makes it fail, with either backend. A program without
  `catchUnwind` still calls `velt_rt_panic` and has no `velt_eh_try`.
- What `_Unwind_RaiseException` returns when nobody claims the exception is no reliable reason
  code: 3 or 5 on macOS, but an arbitrary word on Linux arm64 with libgcc, from a C program too.
  Whether it returns at all depends on the frames above `main` ("What a raise nobody claims
  does"), so the foreign-exception test accepts either outcome: the raise returns and the
  program goes on, or a Rust frame above `main` ends the process; what it checks is that the
  catch never hands the foreign exception back (its payload reads `FOREIGN-CAUGHT`, which a
  catch that claims every class prints).
- Not covered by the prototype: Windows, the JIT, the executor's use (branch `velt-executor`
  has the executor; this branch is off `main`). Not run: x86_64 (macOS and Linux; the Mach-O
  `X86_64_RELOC_GOT` personality relocation and rax/rdx payload registers are written but
  untried).

The foreign exception can't be tested from `velt test`: tests run in a task, and tokio's harness
`catch_unwind` claims it and aborts, which is the Itanium hazard above.

## Before step 5 (the executor's catch)

**Audit: runtime function pointers that run Velt code and are still `extern "C"`.** A Velt
exception through one of them finds no call-site entry in Rust's personality and aborts instead
of reaching the catch. Each becomes `extern "C-unwind"` (an adaptation of the type, no new
code), or the design states why no Velt panic can pass it:

- `DropFn` in `velt_rt/src/drop_depth.rs`;
- `DataFn`, `TransferFn` and `DropFn` in `velt_rt/src/fn_props.rs`;
- `WorkFn` and `DropCtxFn` in `velt_rt/src/native/mod.rs`;
- `DropFn` in `velt_rt/src/transfer_map.rs` (the deferred releases `velt_rt_xfer_end` runs);
- `velt_rt_statics_drop`'s function (`velt_rt/src/entry.rs`, run at exit);
- the entry chain above `velt_main` (`main`, `velt_rt_start`, `run_main`'s `program`): only a
  raise nobody claims reaches it, which the count rules out outside the hole.

Not changed in this PR: nothing that catches today runs through them (the executor's polls do,
after the switch), and each change wants its own measurement.

**Owner decision: drops in unwound frames.** Decided on #2667 (2026-11-09): yes, frames that own
values get cleanup code, only in programs that catch: [Cleanup in unwound frames](#cleanup-in-unwound-frames).
Before it, nothing in the frames between the raise and the catch was freed (#2668 measured 65
blocks and 22 strings per caught panic 20 frames deep), a leak per panic without bound for a
server that answers 500 on a panic.

## Cleanup in unwound frames

Owner decision on #2667: a frame that owns values needing a drop gets cleanup code that runs
when a Velt panic unwinds through it, only in a program whose live code calls `catchUnwind`.
Programs that don't catch stay byte-identical. Gate: run rows and build size within ±3% on
`bench/http` and the async rows with the catch on; the fallback if it fails is a request-scoped
arena (a design of its own).

### Where cleanups come from

Lowering already knows, at every point of a function, what the frame owns: the scope stack of
drop obligations (`lower/drops.rs`: `DropEntry` per scope, the static state or drop flag of each
local), which `return`, `break` and `throw` drop on their edges and an async function's cancel
path drops without its `finally` blocks (`emit_cancel_drops`). A cleanup pad is that cancel
path at a call site: the drops of every open scope, innermost first, without `finally` blocks
and without `catch` handlers (a panic is not an error: user code never sees it), then the
resume of the unwind.

- **Which calls.** `FnLower::call` gives a call an unwind edge when (1) the program catches: sema
  demanded `panic_raise` (the construct `CatchUnwind` is live) and the target unwinds
  (`Catching::of_triple(target) == Itanium`); (2) the callee may unwind: a Velt function or a
  function value (`velt_vir::eh::may_unwind`: `Callee::Func`, `Callee::Ptr`). Externs (the
  runtime, C) don't get one: a panic in a Velt callback the runtime runs leaks the calling
  frame's values, as before; (3) the frame owns something now: some open scope holds an entry
  that would emit a drop (a temporary, a flagged value, a local in state `Init` or with a
  flag, a cell). A frame that owns nothing at a call gets no edge, and a function that never
  owns anything at a call is unchanged. Arguments the call takes are already off the scopes
  (`take_temp` before the call, as for `throw`), so the callee drops them, never both.
- **Pads are shared.** A pad is a chain of links, one per drop entry, innermost first, each
  ending in a jump to the pad of the entries outside it; the empty pad is one block calling
  `velt_eh_resume`. Links are cached per function by the entries and the states they read, so
  consecutive calls in one state share a pad and a new temporary adds one link. Code size grows
  with the number of distinct ownership states at calls, not with calls × entries.
- **Inside a pad** calls get no unwind edge, and the pads are lowered with what lowering knows
  about string data set aside (as a `finally` is), so a pad's code depends only on its entries.
- **Not covered** (they leak as before): calls of externs; the shared `division by zero` panic
  blocks (built after the scopes are gone); Cranelift on Apple arm64 for a call whose arguments
  don't all fit in registers (below); the JIT (`velt dev`); Windows and WebAssembly.

`VELT_UNWIND_PADS=0` turns the edges off in lowering. It is for measuring the gate only (the same
program with and without pads); a program built with it leaks as before #2667.

### VIR

- `Terminator::Call` gains `unwind: Option<BlockId>` (invariant 14). `Some(pad)`: when the callee
  unwinds with a Velt exception, control continues at `pad` with `dest` not written. `None` is
  every call of a program that doesn't catch, so its VIR, fingerprints and code are unchanged.
- `velt_eh_resume()` (`velt_vir::eh::RESUME`, noreturn, a backend-defined extern like
  `velt_eh_try`): continues the unwind that entered the pad being run. Each pad ends in a call of
  it; a call of it never has an unwind edge (verify).
- Passes see the unwind edge as a successor (`velt_opt::visit::successors`, lowering's
  `cfg::successors`, verify's dataflow), so a value a pad reads is live at the call and
  definitely assigned before it (verify checks the pad as any other successor; its check is per
  block, so a pad reading the call's own result is not caught there: lowering never emits one,
  since a call's result is owned only after the call). The inliner wires an inlined body into
  the call's edge: the callee's calls that may unwind and have no edge get the call's, and its
  `velt_eh_resume` calls become jumps to the call's pad; a call without an edge inlines the
  body as it is.
- **Memory kept in locals across calls.** A pass that keeps a pointee's fields in locals and
  stores them back later must also store them where an unwind leaves the frame, since a frame
  further out may own that memory and its pad drops it by what memory says. `noalias` promotion
  stores the written fields in a block on each unwind edge of a call that doesn't receive the
  param (reloading them on the edge of one that does, which may change them), and gives a call
  without an edge one to a block that stores them and resumes, once the program has pads (it
  declares `velt_eh_resume`). `frame_slots` stores a poll function's written slots before
  `velt_eh_resume` as before a `return`. The normal path is unchanged; programs that don't catch
  get the same code. The other passes read memory once (`frozen`, `invariant_reads`, which
  arrives at pads with the pre-call state) or stop at a call with a pad (`fresh`, so
  `dead_fills`); `heap_sroa` only replaces objects no other frame sees.

### Backends

- **LLVM.** A call with an unwind edge is an `invoke ... to label %ok unwind label %lpN`. Each
  pad block reached by an unwind edge gets a landing block of its own,
  `%lpN: landingpad { ptr, i32 } cleanup`, which stores the exception pointer in the function's
  exception slot (an `alloca` in the entry block) and branches to the VIR pad, which other pads
  may also jump to. `velt_eh_resume` is `resume { ptr, i32 }` of the slot. A function with an
  `invoke` names `personality ptr @velt_eh_personality`.
- **Cranelift.** A call with an unwind edge is a `try_call` (`try_call_indirect`) whose default
  handler is a landing block per pad that stores the exception (payload register 0) in a
  variable and jumps to the pad; `velt_eh_resume` calls `_Unwind_Resume` with it. `try_call`
  exists only for the SystemV convention: on Linux and x86-64 macOS that is the platform's; on
  Apple arm64 the call uses a SystemV signature for the same symbol only when every argument is
  in a register (at most 8 integer and 8 float or vector arguments), where Cranelift's
  AppleAarch64 and SystemV conventions agree (arguments are extended by their `sext`/`uext`
  attributes in both); a call with stack arguments stays a plain call. After a function is
  compiled, its call sites with a handler become its LSDA (`.gcc_except_table`, call sites in
  `udata4`, one entry per call: `[return address - 1, +1)`, the handler, action 0 = cleanup);
  its FDE names the LSDA and the CIE with the personality, which the object defines once (local)
  when any of its functions has an LSDA. A `try_call` treats every register as clobbered on the
  exceptional edge, so values live across it are spilled: a debug-build cost only.
- **The personality** becomes the general one (both backends): for Velt's class, read the LSDA
  header (landing-pad base omitted; the type table skipped; call sites in ULEB128 (LLVM) or
  `udata4` (Cranelift's own)), find the entry covering `ip - 1` (`_Unwind_GetIP` minus
  `_Unwind_GetRegionStart`); no entry or landing pad 0: `_URC_CONTINUE_UNWIND`. An entry with
  action 0 is a cleanup: search phase `_URC_CONTINUE_UNWIND`, cleanup phase land
  (`_URC_INSTALL_CONTEXT`). An entry with an action is a catch (`velt_eh_try`): search phase
  `_URC_HANDLER_FOUND`, cleanup phase land only in the handler frame. A foreign class passes in
  both phases, so its unwinding runs no Velt cleanup (unchanged: compiled frames are not
  reached by foreign exceptions today). `velt_eh_try` keeps its LSDA; the general reader finds
  its one entry.

### A panic during a cleanup

A drop in a pad can run user code (`[Symbol.dispose]()`, a class's destructor), which may panic.
Its unwind would run the outer pads of the frame whose pad is running, dropping again what the
inner drop was in the middle of. As in Rust ("panic while panicking"), it aborts: std keeps a
thread-local `UNWINDING` flag, set by `panicRaise` just before it raises and cleared by
`catchVeltPanic` when the catch returns the exception. A `panicRaise` that finds it set prints
the message and `panic while unwinding: aborting` and calls `abort()`. Nothing is added to the
normal path; the cost is that a catch inside a destructor run by a cleanup can't catch either
(it aborts too), which Rust also does.

### Async state machines

A poll function's frame holds its live state in the state struct (`async_fn/spill`): those
values are owned by the future, not the frame, and the panicked state machine is leaked as
before (#2008: never polled or dropped again). So a poll function's pads drop only the frame's
own temporaries: lowering records each pad link of a poll function, and after the spill pass
has chosen what goes into the state, a link whose code touches a spilled local is bypassed (its
first block jumps to the next link). Values that never cross a suspension (the temporaries of
the statement that panicked, locals between two `await`s) are dropped; what the state holds
leaks with it. A plain function's pads drop everything the frame owns.

A generator's resume function (and an async generator's poll function) gives every call that
may unwind a pad, also where the frame owns nothing, and its pads end by setting the state's tag
to finished (`DONE`) before the resume. The frame holding the generator (a `for...of` frame with
the state inline, or the owner of a `Generator` object) then drops it as a finished one, which
does nothing to the state (what it held leaks, as above) and frees the object; without it the
drop would find the tag of a running generator and panic ("generator is already running"), and
during that frame's cleanup the panic aborts. A later `next()` (or `next(v)`) is done, as after
a generator that threw. An async generator's poll function is built the same way; it is reached
by an unwind only once a catch can run inside an executor (the design's step 5). The spill pass's
liveness follows unwind edges, so a bypassed link never moves a local into the state.

### Windows and WebAssembly

No unwinding (Windows until its structured exceptions are built): lowering adds no edges for
those targets, and a backend that gets one anyway (the JIT) emits a plain call.

### Contract changes

- `vir::Terminator::Call::unwind` (vir.rs invariant 14) and the backend-defined extern
  `velt_eh_resume` (`velt_vir::eh::RESUME`, `may_unwind`).
- std: `UNWINDING` in `std/lang/hooks/unwind.velt`, `abort` in the Itanium raise; the leak-check
  marker of a caught panic (`rc stats: std caught a panic`) goes, since a caught panic frees what
  the unwound frames owned.
- Objects of programs that catch: an LSDA per function with pads, the personality's CIE on their
  FDEs, `_Unwind_Resume` and `_Unwind_GetIP` referenced (libSystem, libgcc_s).


## Measurements

Linux arm64 (velt-prof VM), cachegrind, release (LLVM), `VELT_THREADS=1`, base `main`
bcd193394 against the prototype.

**Programs that don't catch: no change.** The prototype changes nothing in a program whose live
code has no `catchUnwind`: the LLVM IR of every row below is byte-identical between base and
head, and the instruction counts differ only by run-to-run noise. The two `timer_await` rows wait
on the wall clock, which moves them by a percent or more between runs of the same binary (#2137
saw −1.5% to +6.3%).

| row | base | head | Δ | LLVM IR |
|---|---:|---:|---:|---|
| json/edit | 120,555,094 | 120,543,149 | −0.01% | identical |
| json/navigate | 2,060,575,662 | 2,060,575,662 | 0.00% | identical |
| json/parse_typed | 2,458,488,111 | 2,458,490,496 | +0.00% | identical |
| json/parse_union | 3,566,916,822 | 3,566,916,822 | 0.00% | identical |
| json/parse_value | 3,047,587,713 | 3,047,521,172 | −0.00% | identical |
| json/stringify | 926,348,528 | 926,347,960 | −0.00% | identical |
| async/await_chain | 230,550,620 | 230,552,985 | +0.00% | identical |
| async/await_deep | 2,486,383,512 | 2,486,389,834 | +0.00% | identical |
| async/hot_loop | 2,642,829,327 | 2,642,821,772 | −0.00% | identical |
| async/fanout_all | 1,027,007,093 | 1,027,007,836 | +0.00% | identical |
| async/fanout_all_throwing | 814,138,943 | 814,134,209 | −0.00% | identical |
| async/all_small_stored | 4,992,950,538 | 4,992,945,812 | −0.00% | identical |
| async/spawn_many | 4,774,335,956 | 4,775,806,411 | +0.03% | identical |
| async/channel_pipeline | 770,200,377 | 770,197,230 | −0.00% | identical |
| async/timers_fire | 567,482,373 | 567,653,633 | +0.03% | identical |
| async/timers_due_join | 147,114,593 | 147,117,008 | +0.00% | identical |
| async/timers_due_stored | 96,077,097 | 96,080,347 | +0.00% | identical |
| async/timers_due_in_promise | 95,494,301 | 95,491,814 | −0.00% | identical |
| async/timers_due_reversed | 122,606,086 | 122,613,828 | +0.01% | identical |
| async/top_level_await | 38,538,306 | 38,535,939 | −0.01% | identical |
| async/timer_await | 282,688,738 | 279,839,657 | −1.01% (clock) | identical |
| async/timer_await_top | 285,712,642 | 281,193,693 | −1.58% (clock) | identical |
| async/promise_identity_loop | 12,919,623,040 | 12,919,722,657 | +0.00% | identical |

**Compile cost.** `velt build --release --emit llvm` of `m1/hello`: 100.13 M → 100.19 M
instructions (+0.06%, best of 4; the hook table entry and the construct). A first version that
re-exported `catchUnwind` from `std/lang/intrinsics`' index, which every program loads, cost
+0.15%: the declaration is imported from its own file instead. `json/parse_value`: within
±0.02%.

**The catch that catches nothing.** 2,000,000 iterations of a closure that adds to a module
variable, called directly (`run(f)`), through `catchUnwind` (`rawCatch`), and through
`catchVeltPanic` (the thread-local count and the null check too):

| | release (LLVM) | per iteration | debug (Cranelift) | per iteration |
|---|---:|---:|---:|---:|
| plain call | 122,393,097 | | 350,566,152 | |
| `catchUnwind` | 144,393,079 | +11 | 346,567,849 | −2 |
| `catchVeltPanic` | 172,393,245 | +25 | 538,572,083 | +94 |

In release the plain call is inlined and the catch is not (`velt_eh_try` is a call of its own
and the closure an indirect call in it): 11 instructions. The wrapper's two thread-local updates
add 14 more; in a debug build a `ThreadLocal` costs a call each. For the executor that is paid
once per poll of a catching promise, against about 75 for a whole suspend and resume in the
executor in Velt (#2649 step 3.1).

**A caught panic.** 10 frames deep, the message printed to stderr: 73,907 instructions in
release, 89,080 in debug (the difference of 20,000 and 10,000 caught panics). Most of it is the
two-phase search and the cleanup phase through libgcc's DWARF unwinder, and the message's text.
It is a cold path; the same panic uncaught ends the process.

**Size.** Per object that catches: `velt_eh_try` and the personality (a few hundred bytes of
code), a 15-byte LSDA, one more CIE; an 8-byte personality pointer on ELF.

## Questions for the owner

1. **Personality.** The prototype's minimal one (one landing pad per frame, no LSDA search,
   `velt_eh_try` never inlined), or a general LSDA reader in Velt so the helper can be inlined?
   *Recommended: the minimal one*; revisit only if the call measures in an executor row.
2. **Where the panic path runs.** Only in programs that call `catchUnwind` (prototype: no cost to
   any other program, not even compile time), or always in std from the switch on, deleting
   `velt_rt_panic`? *Recommended: on demand now; always at the switch*, when the runtime's catch
   is deleted and every server program catches anyway.
3. **The payload.** The message only (prototype: a NUL-terminated copy), or `{ VeltStr, location
   }` once `addressStore` lands, or also a value (`panic(value)`)? *Recommended: the moved
   `VeltStr` and the location*; a value payload has no use case yet.
4. **Windows.** Fixed machine code for `velt_eh_try` with a hand-written SEH scope table, shared by
   both backends, or LLVM's SEH lowering plus fixed code for Cranelift only? *Recommended: fixed
   code for both*: one implementation to test, and nothing to inline on a path that already pays
   a call.
5. **The thread-local count after the switch.** Keep raising only inside a catch (an uncaught
   panic exits without walking the stack, and a stray Rust `catch_unwind` can't abort the
   process), or always raise and exit when nobody claims it? *Recommended: keep it*: two
   thread-local updates per catching poll.
6. **`velt dev`.** Register the LSDA with JIT code before the executor switch (handlers in `velt
   dev` keep answering 500 on a panic), or accept that a panic ends a `velt dev` server until
   then? *Recommended: register it*; it is the same bytes `__register_frame` already takes.
