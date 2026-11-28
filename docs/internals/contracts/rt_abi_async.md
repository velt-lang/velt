# Runtime C ABI, part 2: async, std/fs, std/net, process, shared state, strings & JSON

Implemented in `crates/velt_rt` (native targets) and `crates/velt_rt_wasm` (WebAssembly), tested
from Rust with hand-written state machines in `crates/velt_rt/tests/abi/` and from C in
`crates/velt_rt/tests/link_check.rs`. It changes only through a reviewed contract change.
Everything from [rt_abi.md](rt_abi.md) holds (`bool` is `u8`, `VeltStr` = static / inline /
counted heap string, see rt_abi.md "Strings"). All functions are `#[no_mangle]`, `extern "C"` or
`extern "C-unwind"` (§1.5); VIR signatures are scalar/pointer only. Every `velt_rt_*` name in a
table of this file is exported by velt_rt and listed in the JIT host's symbol table
(`crates/velt_rt_host/tests/abi_symbols.rs` checks it); a symbol only velt_rt_wasm defines is
named in prose.

Semantics follow https://veltlang.com/docs/reference/async/: a stored promise **starts when it
is created** (`velt_rt_fut_start`, §1.1) and runs as a local promise of the task that created it;
directly awaited calls are embedded state machines.

**Which executor runs the tasks.** On a native target the executor is written in Velt
(std/lang/hooks/sched*.velt): lowering calls its hooks in place of the task and promise
functions of §1 and §2 (velt_vir `lower/sched.rs`; the externs std declares are listed in
velt_common `std_hooks::SCHED_EXTERNS`), and the hooks keep the semantics this file states. The
executor's workers use the runtime's leaves (§5, §6, §14) through the C waker of §1. On native
targets velt_rt keeps only the promise functions a program without the executor still calls
(`velt_rt_fut_share`, `velt_rt_fut_id`); a promise shared outside any task (a
C library's thread) is refused (#3782).
A WebAssembly program calls the functions of §1 and §2 directly (velt_rt_wasm's executor).

## 1. Awaitable protocol

```c
typedef uint32_t (*PollFn)(void* state, void* cx);   // 0 = PENDING, 1 = READY
typedef void     (*DropFn)(void* state);

typedef struct VeltFut {                             // every runtime-owned future (16 bytes)
    uint32_t (*poll)(struct VeltFut* self, void* cx);
    void     (*drop)(struct VeltFut* self);
} VeltFut;                                           // result slot at byte offset 16

typedef struct VeltWakerHeader VeltWakerHeader;     // a block starting with its table
typedef struct VeltWakerVt {                         // a waker's C functions
    const VeltWakerHeader* (*clone)(const VeltWakerHeader* w); // one more reference
    void (*wake)(const VeltWakerHeader* w);          // wakes, gives a reference back
    void (*wake_by_ref)(const VeltWakerHeader* w);   // wakes, keeps the reference
    void (*drop)(const VeltWakerHeader* w);          // gives a reference back
} VeltWakerVt;
struct VeltWakerHeader { const VeltWakerVt* vt; /* the waker's own fields follow */ };

typedef struct VeltCx {                              // what `cx` points to (2 pointers)
    const uint32_t*        await_word;               // offset 0: read by generated code
    const VeltWakerHeader* waker;                    // the task's waker, borrowed for the poll
} VeltCx;
```

**Compiled `async function`** (emitted by lowering):
- A state struct, align ≤ 16, whose **result `T` is at offset 0** (the first field). The rest
  (resume tag, live locals, embedded child states, heap-future pointers) is the compiler's business.
- `uint32_t <f>$poll(void* state, void* cx)`: runs until the next suspension; returns 0 after the
  awaited child returned 0 (the child registered `cx`'s waker), or 1 after writing the result.
  Must not be called again after returning 1.
- `void <f>$drop(void* state)`: drops the live locals of the current suspension point
  (including pending children / heap futures). **Never drops the result slot.** The runtime calls it
  exactly once iff a future is dropped before completing (cancellation); after READY nobody calls it
  — the awaiter moves the result out.
- Before the first poll a state may be moved/copied bytewise (the runtime copies initial states
  into tasks); it must not contain pointers into itself until it has been polled. After the first
  poll it never moves.
- `cx` points to a `VeltCx`: the task's **await word**, then its **waker**, one pointer to a
  header whose first word is its table (tokio lays out its task headers the same way). A
  task header starts with it; a C library's waker is any block that starts with its own table.
  Generated code passes `cx` on and reads only the word (below, "Awaiting"). The waker contract,
  for every table:
  - The waker in `cx` is borrowed for the poll call: a callee may `clone` it and `wake_by_ref`
    it, never `wake` or `drop` it. A reference from `clone` ends in exactly one `wake` or `drop`.
  - All four functions may be called from any thread, including threads Velt did not start
    (tokio's, a GPU driver's callback): on the borrowed waker during the poll, on a reference
    from `clone` at any time.
  - The reference count is atomic.
  - A wake after the task finished is harmless: the count keeps the header alive until the last
    reference is given back.

  The Rust executors pass a header (on the stack, beside the `VeltCx`) with the runtime's own
  table and their `&mut Context`; runtime leaves recognize the table and use that `Context`
  as it is (velt_rt `task/waker.rs`). A `clone` through that table returns a boxed header
  owning a Rust `Waker`. For any other table a leaf's Rust waker is `RawWaker::new(header,
  &FORWARD)`, whose entries call the header's table: nothing is allocated on a clone.
- Generators (`function*`) reuse this state-machine shape without the runtime: their poll
  function is called with a null `cx` and returns 0 (done), 1 (a value is in the result slot)
  or 2 (the slot holds the `Err`), and `$drop` closes a suspended generator (it runs `finally`
  blocks). Async generators (`async function*`) are polled with the awaiting function's `cx`
  and return 0 (pending: the waker is registered, as for any poll), 1 (done), 2 (a value) or 3
  (an `Err`); setting tag bit `0x4000_0000` (`$close`) and polling to completion closes one,
  awaiting its `finally` blocks. Nothing of this crosses the runtime ABI either (velt_vir
  `async_fn/generator.rs`): the runtime never sees an async generator, only the futures of
  the functions that drive it.

**Awaiting** (inside a poll function):
- *Compiled child, fast path (no allocation):* the child state is a field of the parent state.
  Initialize it, then `r = child$poll(&st->child, cx); if (r == 0) return 0;` and read the result
  at `&st->child + 0`.
- *Heap future `f` (leaf, join handle, boxed promise):* `if (f->poll(f, cx) == 0) return 0;`
  then move the result out of `(char*)f + 16` (type given per function below), then
  `f->drop(f)` (cancels it if still running and frees it; never touches the result). Compiled
  code calls through the header itself, with the C calling convention (unwinding allowed); the
  runtime has no function for either. On wasm32 each header entry is an 8-byte slot holding the
  function's table index in its low 4 bytes, as every pointer slot there.
- *The step after an `await`* (user code; std code takes none, its prelude counts Node's steps
  with `velt_rt_microtask`): once the child or heap future is READY, the code after the `await`
  runs one microtask later, as in Node. `if (*cx->await_word != 0) { t = velt_rt_await_tick(cx); if
  (t != 0) { st->ticket = t; suspend; on resume: if (!velt_rt_await_ready(st->ticket, cx))
  return 0; } }`. An inline `await Promise.all([…])` takes two such steps once it settles (Node
  settles the joined promise in the reaction it queues on the deciding input, and the `await`'s
  step follows). The result stays where it is (child state offset 0, or moved out of the heap
  future before it was freed) and is dropped by the cancel path of that suspension. The word is
  0 while no other job of the task could run first (its microtask queue is empty and no
  synchronous run is under way: a started promise's first run, a promise run from inside another
  poll), so `await`-only code pays one load and branch. A program that calls none of
  `velt_rt_fut_start`, `velt_rt_microtask`, `velt_rt_latch_open`, `velt_rt_fut_detach`,
  `velt_rt_race`, `velt_rt_race_ok`, `velt_rt_all_or_reject` can never make the word non-0;
  lowering drops its checks (velt_vir `async_fn/await_order.rs`).
- Every runtime function returning `VeltFut*` allocates exactly that object and does no work until
  the first poll, except that a timer's deadline is fixed at creation (like Rust's `sleep`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_fut_box` | `(PollFn, DropFn, const void* state, u64 size, u64 align) -> VeltFut*` | WebAssembly only (velt_rt_wasm) [velt_rt's removed: native programs box, start and hand over promises on the executor in Velt, std/lang/hooks/sched_node.velt, sched_node_transfer.velt]. Moves a compiled initial state into a heap future (for `Promise<T>` values that are stored, put in arrays, returned); result at +16 = state offset 0. `align ≤ 16`. Lazy until polled or started (§1.1). |
| `velt_rt_fut_start` | `(VeltFut* f, void (*result_drop)(void* slot))` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, as `velt_rt_fut_box`]. Start a boxed promise now (§1.1); no-op for any other future, or outside a task. `result_drop` disposes of an unclaimed result (null if nothing to do); for a promise that can reject it reports an `Err` like an unhandled rejection. |
| `velt_rt_fut_start_rejecting` | `(VeltFut* f, void (*result_drop)(void* slot))` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs start, give up and hand promises to combinators on the executor in Velt, std/lang/hooks/sched_node.velt, sched.velt `schedDetach`, `schedHandled`]. `velt_rt_fut_start` for a promise that can reject (its result is a `Result`, tag first; `result_drop` reports an `Err`): when it rejects while its owner does not wait for it, the rejection is reported as uncaught (through `result_drop`; on WebAssembly it then gets 0 from `velt_rt_unhandled_defer`) once the task's microtask queue is drained, unless the owner awaited it, dropped it, or handed it to a combinator or another task by then, as Node reports a rejection without a handler. |
| `velt_rt_fut_transfer` | `(VeltFut* f, void (*transfer)(void* slot))` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, as `velt_rt_fut_box`]. The owner hands `f` to another task: `transfer` (compiled transfer glue, in place; null for a result with nothing counted, which only hands over a holder of a shared promise, §1.2) runs on the result slot on the task that produces the result, as it finishes (at once if a started promise already finished); with or without `transfer`, a started promise's rejection is no longer reported by the task that started it (`velt_rt_fut_start_rejecting`) but claimed or given up by the task holding it (#1497); a race passes it to its children; no-op for join handles (a task transfers its own result: `velt_rt_spawn_transfer`, or the mark of the promise `velt_rt_spawn_fut` runs) and runtime leaves. WebAssembly does the same on its one thread, so values are moved or copied where native targets move or copy them. |
| `velt_rt_yield_now` | `(void* cx)` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs yield on the executor in Velt, std/lang/hooks/sched_yield.velt `schedYield`, which returns the yield's token (`POLL`, 0: it has no record and goes on once the poll its task's root yielded it in is over, which the task's `YIELDED` state marks, whatever cx asks, or it was made off the workers; not 0: its held record on its task's yield list). A cx is told apart by its address: a yield with the task's own cx (`T_CX`) is the root's, one poll's first with no yield block in use, and takes one test; a child polled with a copy of its parent's cx (the same two words at another address) takes the task path that orders several yields of one poll. Only the inline `Promise.all` polls its children so (velt_vir async_fn/all.rs `child_cx`); everything else passes its own cx or the one it was given: `await yieldNow()` keeps it across its suspension, hands it to `schedYieldReady(cx, record)` when it resumes (it goes on once that record's turn came, whatever cx polls it), and to `schedYieldDrop(record)` from its cancel, #3611, #3687; so does `velt_rt_yield_now_fut` below, `schedYieldFut`]. `await yieldNow()` inline: call it, then `return 0`; resumes after other ready tasks. No allocation. |
| `velt_rt_yield_now_fut` | `() -> VeltFut*` | WebAssembly only, as `velt_rt_yield_now`. `yieldNow()` as a value; result: none |
| `velt_rt_await_tick` | `(void* cx) -> u64` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs take `await` steps and queue microtasks on the executor in Velt, std/lang/hooks/sched_micro.velt]. The step of an `await` when the word is not 0: 0 to go on now (nothing else can run first, or the waiter's step was queued by `velt_rt_latch_open` already), else the ticket of an entry queued at the back of the task's microtask queue for `cx`'s waker. No allocation. |
| `velt_rt_await_ready` | `(u64 ticket, void* cx) -> u32` | WebAssembly only, as `velt_rt_await_tick`. 1 once the queue reached `ticket` (or for a ticket of another queue); 0 while it waits, and the entry then resumes whoever polls with `cx` now. |
| `velt_rt_microtask` | `() -> VeltFut*` | WebAssembly only, as `velt_rt_await_tick`. Waits for its turn in the microtask queue of the task that polls it (`then` / `catch` / `finally` callbacks and `queueMicrotask`, std/prelude/promise.velt); result: none. The queue is first in, first out and is drained at every job boundary of the task: after the root's poll and after each local promise the task runs from its ready queue (entries queued meanwhile run in the same drain). An entry runs its waiter directly: a local promise of the task is run, the root is polled, any other waker is woken; an entry whose promise is driven by its owner by then is skipped (the owner's own entry resumes it). Outside a task it is ready at once. WebAssembly keeps one queue that runs before the ready tasks. `await` steps (`velt_rt_await_tick`) use the same queue. No code pointers are stored (§13.5). |
| `velt_rt_unhandled_defer` | `() -> u8` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: on native targets the executor in Velt calls the function only once the rejection is to be reported, with the throw's location already set, std/lang/hooks/sched_node_report.velt and sched_join.velt]. Called by the function that disposes of an unclaimed rejecting promise's result (`velt_rt_fut_start`'s `result_drop`) before it reports an `Err`: 1 when the runtime keeps the promise to report it once the task's microtask queue is drained, as Node does (the function then returns without reporting or dropping; the runtime calls it again then, and it gets 0); 0 to report now (outside a runtime drop of a started promise in a task). |
| `velt_rt_uncaught_now` | `()` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs keep the location in std, std/lang/hooks/sched_exit.velt `uncaughtNow`, and restore it in sched_node_report.velt `dropResult`]. The next rejection nobody handles is an error a timer or microtask callback threw (the prelude's `uncaught`): it is reported at once, as Node's uncaught exception, with the location where the callback threw, not where the prelude throws it again. |
| `velt_rt_task_gate` | `() -> u64` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: nothing called it since the prelude's callbacks run on the creating task]. A counted handle to the gate of the task being polled (0 outside a task): a callback that runs on another task can wait with it for the end of its creator's poll (std/prelude/timers.velt no longer needs it: its callbacks run on the creating task). Released with `velt_rt_task_gate_free`. WebAssembly: always 0. |
| `velt_rt_task_gate_wait` | `(u64 gate) -> VeltFut*` | WebAssembly only, as `velt_rt_task_gate`. Completes once the poll the gate's task was in when the wait was first polled has ended (each poll ends with the task's microtask queue drained), at once when the task is not in a poll or for gate 0; so a timer's callback never runs before the callbacks its creator queued. Result: none. |
| `velt_rt_timer_new` | `(i64 ms) -> u64` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs order their timers on the executor in Velt, std/lang/hooks/sched_order.velt, which lowering calls in place of these externs]. A place in the process-wide order of timer callbacks (`setTimeout`, `setInterval`, `setImmediate`; std/prelude/timers.velt) for a timer of the task being polled, as Node orders timers: at the end of the task's list for delay `ms` (negative = 0); a new list's expiry is the monotonic clock read now in whole milliseconds plus `ms`. The lists of all tasks run by (expiry, creation of the list); a pass over the due timers reads the clock once and runs each due list's timers due by then in creation order, and a list whose next timer is not due by then waits for it with a new place (Node's `listOnTimeout`). `ms` 0 (or less) is an immediate (`setImmediate`): the task's immediates are not in a list; once a pass finds nothing more due, the immediates created by then run, in creation order, before the next pass reads the clock (Node's check phase). The turn waits at least `ms`. Insert, removal and finding the next timer take logarithmic time. Freed with `velt_rt_timer_free`. |
| `velt_rt_timer_turn` | `(u64 timer, u64 gate) -> VeltFut*` | WebAssembly only, as `velt_rt_timer_new`. Completes once the timer is due, the poll `gate`'s task (0: none; the prelude passes 0, its callbacks run on the creating task) was in when it came due has ended (as `velt_rt_task_gate_wait`), it comes next in the order among the timers whose task is not busy in a poll of another task (a busy task's lists are passed over as a whole), and the poll of the task that took the previous turn has ended (each poll ends with its microtask queue drained); the timer then leaves the order. So callbacks run in Node's order, each followed by its microtasks, across tasks. Ready at once for a timer that is no longer queued (`velt_rt_timer_clear`). Called in the first run of a started promise (the prelude's), it marks that promise as a timer's: it does not keep an orphaned set alive (§1.1; a ref'd timer holds a keep-alive reference itself). WebAssembly: the same lists, fired by the executor, and `block_on` does not wait for such promises. Result: none. |
| `velt_rt_timer_again` | `(u64 timer, i64 ms)` | WebAssembly only, as `velt_rt_timer_new`. Queues the timer again at the end of its list, due `ms` after the clock read when it last took its turn (an interval's next run, as Node's: called after the callback's synchronous part). |
| `velt_rt_timer_clear` | `(u64 timer)` | WebAssembly only, as `velt_rt_timer_new`. Takes the timer out of the order (`clearTimeout`; a no-op once it took its turn or was cleared); its turn completes at once. May be called from any task while the handle is live (the prelude calls it under the timer state's lock, which also covers `velt_rt_timer_free`). |
| `velt_rt_timer_free` | `(u64 timer)` | WebAssembly only, as `velt_rt_timer_new`. Takes the timer out of the order if it is still queued (its task ends without a turn) and frees the handle. |
| `velt_rt_task_gate_free` | `(u64 gate)` | WebAssembly only, as `velt_rt_task_gate`. Releases a gate handle (0: no-op). |
| `velt_rt_all` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results) -> VeltFut*` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs join on the executor in Velt, std/lang/hooks/sched_all.velt]. `Promise.all(array)`: takes ownership of the `n` futures (not of the pointer array); child `i`'s result is moved to `results + i*result_size` (must stay valid until completion/drop); concurrent, only woken children are re-polled. Result: none. |
| `velt_rt_all_with_drop` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | WebAssembly only, as `velt_rt_all`. Same as `velt_rt_all` (which = this with `result_drop == NULL`), for results that own resources: if the returned future is dropped **before completing**, `result_drop(results + i*result_size)` runs for every child `i` that had already finished (pending children are cancelled via their own drop). After completion it never runs — all results belong to the awaiter. Use it whenever `T` needs dropping. |
| `velt_rt_all_or_reject` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | WebAssembly only, as `velt_rt_all`. `Promise.all` over promises that can reject: like `velt_rt_all_with_drop` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled), but completes as soon as a child rejects. Its result is empty. On a rejection the rejected child's result is moved to slot 0 and is the only initialized slot (so a rejection is an `Err` tag in slot 0, `n > 0`): the runtime drops the other finished results with `result_drop` (null: nothing to drop) and drops the pending children (started promises keep running; mark them with `velt_rt_futs_handled` first). |
| `velt_rt_race` | `(VeltFut* const* futs, u64 n, u64 result_size) -> VeltFut*` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs race on the executor in Velt, std/lang/hooks/sched_race.velt]. `Promise.race(array)`: takes ownership of the `n` futures (not of the pointer array); the first child to finish moves its `result_size`-byte result to the returned future's slot (+16) and the others are dropped (started promises keep running, §1.1; one created during the task's current poll that is queued, woken by that poll's own code, runs once now, before the race's awaiter; so do the pending children of `velt_rt_all_or_reject` when it rejects). `n == 0` never completes. |
| `velt_rt_race_ok` | `(VeltFut* const* futs, u64 n, u64 result_size, void (*reject_drop)(void* slot)) -> VeltFut*` | WebAssembly only, as `velt_rt_race`. `Promise.any`: like `velt_rt_race` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled): the first fulfilled child wins; a rejected one is dropped with `reject_drop` (null: nothing to drop) while others are still running, and the last rejection is the result when all reject. |
| `velt_rt_fut_detach` | `(VeltFut* f, void (*quiet_drop)(void* slot))` | WebAssembly only, as `velt_rt_fut_start_rejecting`. The owner gives up `f` without cancelling it (pending siblings of an early `Promise.all` rejection). A boxed promise that is still lazy, even one its owner has polled, becomes a started promise of the current task. If it was created during the task's current poll (and nothing yielded during it), it is polled once now, before the owner's continuation: only that poll's own code can have woken it, as with a JS microtask queued before the rejection handler (#150). Otherwise it runs at the task's next poll, after the owner's continuation, as in JS, where the rejection handler runs before the timers and events that woke it. A started promise keeps running, as a loser of `velt_rt_race` does (§1.1). Either way its result is disposed of with `quiet_drop` (handled, null: nothing to drop). Any other future is dropped (`f->drop(f)`). Takes ownership of `f`. |
| `velt_rt_fut_share` | `(VeltFut* f, u64 result_size, void (*share)(const void* src, void* dst), void (*result_drop)(void* slot)) -> VeltFut*` | another holder of promise `f` (§1.2): a new handle on `f`, which is now shared; null stays null. `share` writes another reference to a result into a slot (null: the bytes are copied), `result_drop` drops one (null: nothing to drop) |
| `velt_rt_fut_id` | `(VeltFut* f) -> u64` | the identity of promise `f` (§1.3): what `===` and `Set` / `Map` keys compare and hash for a promise; the same for every holder of a shared promise; null gives 0. Natively only std's `schedFutId` calls it, for a holder of the runtime's hub (a promise shared on a C library's thread, #2196 9e); WebAssembly also follows converted promises (below) |
| `velt_rt_fut_alias` | `(VeltFut* w, VeltFut* inner, void (*quiet_drop)(void* slot))` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196 9e: natively std's `schedFutAlias`, std/lang/hooks/sched_share_alias.velt]. Converted promise `w` (from `velt_rt_fut_box`, not yet run) wraps or awaits `inner` (a holder, borrowed): `w` stands for the promise `inner` holds when that one is shared (§1.3); `quiet_drop` drops `w`'s result without a report (null: nothing to drop). No-op for anything else |
| `velt_rt_fut_views` | `(u8 on)` | WebAssembly only (velt_rt_wasm, a no-op there) [velt_rt's removed, #2196 9e: natively std's `schedFutViews`]. The holders `velt_rt_fut_share` makes until it is called with 0 are *views* (§1.3), through which a converted promise awaits the promise it stands for |
| `velt_rt_fut_peek` | `(VeltFut* f) -> u8` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196 9f: natively std's `schedPeek`, std/lang/hooks/sched_peek.velt, and `0` with no call in a program without the executor, where nothing is awaited]. What `console.log` shows of a promise its owner holds: `1` when its result is in the slot (+16; a `Result` tag first for a promise that can reject), `0` while pending. Polls, claims and moves nothing. A future that only runs when awaited (a runtime leaf, a join handle, a combinator, a promise created outside a task) reads as pending |
| `velt_rt_futs_handled` | `(VeltFut* const* futs, u64 n, void (*quiet_drop)(void* slot))` | WebAssembly only, as `velt_rt_fut_start_rejecting`. The `n` futures are handed to a combinator (`race`, `any`, `all`), which handles their rejections like JS: a started promise among them that is dropped unfinished later disposes of its result with `quiet_drop` (null: nothing to drop) instead of its `result_drop`, so a rejection is not reported (§1.1). No-op for other futures. Called before the combinator takes the futures. |

Bounded drop nesting (#543) is std's (std/lang/hooks/drop_queue.velt, #2196): a drop that can nest (glue whose type leads back to itself) counts itself in this thread's state word, std's `STATE` (`ThreadLocal<u32>`), which compiled code reads and writes inline as a `velt_tls_*` value (rt_abi.md "Thread-local values"; velt_vir `tls::DROP_STATE_SLOT`): bits 0..29 count the drops under way, bit 30 says something was queued, bit 31 that the queue is being drained. Per drop: `n = STATE; if (n & 0x3fffffff) >= 128 { dropQueue(value, drop) } else { STATE = n + 1; <drop>; m = STATE - 1; STATE = m; if m == 1 << 30 { dropDrain() } }`. `dropQueue` keeps `value` and its drop glue (a closure env's drop, a field or struct value moved to a heap box, or a box with its last reference) for the end of the outermost drop and sets the queued bit; `dropDrain` drops the queued values, which may queue more, in the order they were queued, then gives the queue's block back and clears the word. A caught panic (std/task/unwind.velt) restores the word and drops what is still queued (`dropRecover`).

`Promise.all([a(), b()])` with a static list should be compiled inline instead (children embedded,
each polled until done, done-flags in the state): no allocation.

### 1.1 Started promises (hybrid promises)

`const p = f()` compiles to `p = f(args)` (the boxed constructor, `velt_rt_fut_box`) followed by
`velt_rt_fut_start(p, result_drop)`. Direct `await f()` and `spawn(f())` never box or start.
- Starting runs the state's poll right away (so `f` runs until its first suspension, like JS)
  and adds `p` to the **local set** of the task being polled. Every task root (`block_on`,
  `velt_rt_spawn*`, HTTP handler requests) first polls its woken local promises, then its own
  state; local promises of a task are never polled concurrently with each other or with the
  task (they may move between workers with it). A local promise's poll gets a waker that queues
  it in its set and wakes the task. When one finishes, its awaiter runs at once if it is the
  task's root or another promise of the set (a JS microtask), else it is woken.
- Polling a started promise (`f->poll`): READY once it finished (the awaiter then owns the
  result at +16). Polled from inside the task that drives it while it is not woken, the awaiter
  polls its state directly with its own `cx` ("adopts" it: its leaves then wake the awaiter, with
  no trip through the set — e.g. `Promise.all` over stored promises); otherwise the awaiter's
  waker is registered and the set drives it. Any task may await it. An adopted promise that is
  dropped, or awaited from elsewhere, goes back to its set.
- Dropping an unfinished started promise (`f->drop`) does **not** cancel it: it keeps running
  and `result_drop` disposes of its result when it finishes (`quiet_drop` instead once
  `velt_rt_futs_handled` marked it). Dropped after finishing without being
  awaited, the result is dropped then. The node is freed when the owner, the set and every cloned
  waker let go of it (reference count).
- If a task's root finishes while local promises are unfinished, they move to an orphan task that
  drives them to completion and holds a keep-alive reference (the program entry waits, §7) while
  any of them is not a timer's (`velt_rt_timer_turn`). A
  cancelled task (dropped before finishing, e.g. an HTTP request whose client disconnected)
  cancels its unfinished local promises (their `DropFn` runs); awaiting one of those afterwards
  is a fatal error.
- Outside a task (no `block_on` running), `velt_rt_fut_start` does nothing: the promise runs
  when it is awaited or spawned. A synchronous `main` of a program that calls `velt_rt_fut_start*`
  or `velt_rt_spawn*` anywhere runs inside `velt_rt_block_on` (a root that calls `main` and
  stores its result at `state+0`), so this happens only in module initializers.
- Layout: a boxed future is `[head (96 bytes)][VeltFut hdr][state]`; the head holds the state's
  poll/drop, `result_drop`, the reference count, flags, the set and the awaiter's waker. Code
  pointers live only there and in the `VeltFut` header (hot-reload rule, §13.5).

### 1.2 Shared promises

A promise is a shared value (https://veltlang.com/docs/reference/async/ "Promises"): where sema
shares a promise place (`Intrinsic::Share`, or a clone of a value holding a promise), lowering calls
`velt_rt_fut_share(f, ...)` and gives the new holder the future it returns. A program that never
shares a promise never calls it, and its futures are unchanged.
- The first share moves the original future's header entries to a side record (the hub, found by
  the future's address) and replaces them with the runtime's own. The original pointer stays its
  first holder's. Every share returns a new handle: a small heap future with a result slot at
  +16, with the hub in front of its header. Each holder is awaited and dropped as one future.
- Awaiting is unchanged for generated code: poll until `READY`, move the result out of +16, drop.
  Before a holder returns `READY`, it writes a fresh share of the settled result into its slot,
  using the `share` glue. That gives the same object for an object result, and a copy of a
  string or a value.
- A started original future is driven by its task's set: the hub awaits it with a waker of its
  own, and when it finishes every waiting holder is queued in the task's microtask queue (as
  `velt_rt_latch_open` queues its waiters) in the order they started waiting, as Node queues a
  promise's reactions. `velt_rt_race` over a holder that settled takes it before a child that
  only finishes when the race polls it. Any other original is driven by the first holder that
  waits, with its own `cx`; when it settles there, that holder goes on as the awaiter of an
  unshared promise would, and the others are queued the same way.
- Dropping a holder never cancels the promise. The last holder drops the settled result with
  `result_drop`. If the promise has not settled, the last holder restores the original header
  and drops the future, which then behaves like any dropped promise: a started promise runs to
  completion, and its own result drop reports an unhandled rejection. A waiting holder that is
  dropped wakes the others, so the first of them drives the promise.
- The runtime functions that recognize boxed promises by their header (`velt_rt_fut_start`,
  `velt_rt_futs_handled`) treat a holder as some other future. A peek (`console.log`) reads the
  settled result.
- `velt_rt_fut_transfer` of a holder (the holder goes to another task: `spawn`, a channel, a
  closure capturing it) hands it over: a new holder awaits the promise on this task inside a
  started promise of its own (the proxy), whose result `transfer` moves or copies for the other
  task as for any promise handed over (#160); a null `transfer` (a result with nothing counted)
  copies it as it is. The holder handed over forwards its polls and its drop to the proxy, so
  the other task gets a copy made on the promise's task. On the other task the holder is a
  promise of that task: `velt_rt_fut_share` of it shares its proxy there, and handing it on
  again hands over its role in that promise the same way.
- Every holder has the same `poll` and `drop` entries, which say only that a future is shared.
  An original future is found in the runtime's table of hubs by its address, and a handle never
  is.
- A shared promise stays on the logical task where it was shared (its task id: the
  orphan task driving a task's promises after its root returned has the same one; on
  WebAssembly, the task a started promise runs for). A holder that is polled, shared, peeked or
  dropped on another task without having been handed over (above) stops the program with a
  fatal error (exit code 101), unless the thread holds the module lock (§ module state; on
  WebAssembly: the program has module state). That is all the check tests. It is sound because
  the task the promise is shared on holds the lock whenever it runs too: a holder reaches
  module state only through code holding the lock, so the task it is shared on put it there
  or first used it from there, and is a *touched* task from then on, whose every poll takes
  the lock. So
  the tasks using a holder read from module state (a `Map` of promises in flight), its
  result, and the objects reached from that run one at a time.
- On native targets a promise shared on a C library's own thread (in an exported function it
  calls) stops the program with a panic naming #3782, whatever the executor's workers run
  meanwhile and before the executor started too, until such a share can be handed to the
  executor in Velt (std/lang/hooks/sched_share.velt `offWorker`). velt_rt's own hub serves only
  a program that runs nothing on the executor: its promises are runtime futures, driven by
  their first waiter.
- A started original's result is claimed (`OWNER_DELIVERED`) only under the hub's lock, by the
  hub's waker on the promise's task or by the holder that saw it finish, whichever comes first;
  the other finds it settled.

### 1.3 Promise identity

JS compares promises by reference (#1682). Each holder of a shared promise is a pointer of its
own, so lowering compares and hashes promises with `velt_rt_fut_id` (natively std's hook
`schedFutId`, std/lang/hooks/sched_share.velt, which asks the runtime only about a holder of the
runtime's hub, #2196 9e): the original future of a shared promise (it stays allocated while any
holder lives), the future itself for an unshared one (the same pointer once it is shared, so a
hash taken before stays valid). A shared promise that isn't a node of the executor in Velt (a
join handle, a `sleep` kept as a value, a runtime leaf) keeps its own address too: it becomes
the holder of a node that drives it, whose hub keeps it allocated while any holder lives
(std/lang/hooks/sched_share_leaf.velt).
- A converted promise is a new future: a `Promise<T, E1>` as a `Promise<T, E2>`
  (`Intrinsic::PromiseWiden`, a wrapper owning `inner`), or a `Promise<T>` as a
  `Promise<unknown>` (`Intrinsic::PromiseSame`: a `then` converting the value, which awaits a
  holder of the promise). Lowering marks it with `velt_rt_fut_alias` before it runs: when the
  promise it converts is shared, it keeps a holder of that promise of its own until its owner
  drops it, and its identity is that promise's.
- Its rejection is that promise's, reported once. A `then` converting a value awaits the
  promise through a view (`velt_rt_fut_views` around its creation), whose taking the result
  handles nothing; when the converted promise's rejection is unclaimed at the microtask
  checkpoint, it is reported only if no other holder of the promise took the result, and
  dropped with `quiet_drop` otherwise. Taking a converted promise's result (its owner, a
  holder of it, a combinator) counts as taking the promise's, so every other conversion of it
  is quiet too.
- Identity stays on the task: a converted promise handed to another task
  (`velt_rt_fut_transfer`) or given up (`velt_rt_fut_detach`) gives up its holder and record
  there, on its own task, and a lazy marked node is never started by `velt_rt_fut_start`. A
  holder handed over is its proxy's promise on the other task.
- WebAssembly keeps identities but has no views: a
  converted promise's rejection is reported as its own there.

### 1.4 Host promises (JS hosts)

The call of an async host function (`declare host("m") async function f(…): Promise<T>`,
https://veltlang.com/docs/reference/host-functions/) is the import `m.f`, which calls the JS
function and returns a `u32` id under which the glue keeps its JS promise, then a promise leaf over
that id (velt_rt_wasm `host_promise.rs`), and the promise of a std hook awaiting the leaf (velt_vir
`lower/host.rs`). The leaf's result is std's `HostSettled<W>` (std/host/promise.velt) in its foreign
layout, `{ VeltStr code; VeltStr message; W value; }` (offsets 0, 24, 48): `W` is `T`, `u32` (a
`HostRef` slot, 0 for `null`) for `HostRef` and `HostRef | null`, and `bool` (unused) for `void`.
The hook throws `IoError(code, message)` when `code` is not empty, else makes the `T` of `value`;
the call's promise is a `Promise<T, IoError>`, started like a compiled async function's call (§1.1).

`VeltFut* velt_rt_host_promise(u32 id)` (velt_rt_wasm only, wasm32-unknown-unknown): the leaf of
host promise `id`; result `HostSettled<W>`. Pending until the host settles the promise
(`velt_settle`, §2.5); polled then, it calls the import `velt.host_take(id, slot)`, which writes
the outcome to the zeroed slot (the value as the import's result convention writes a result of
its type, or the rejection's name and message as strings) and forgets the promise. Dropped before
that, it calls `velt.host_forget(id)`.

### 1.5 Promises that catch panics (`catchPanic`)

`velt_rt_panic` (rt_abi.md) ends the process unless the thread is polling a started promise that
catches panics. Then it prints the message as usual and unwinds (a Rust unwind with a private
payload) through the compiled frames to the runtime's poll of the nearest such promise. Compiled
functions therefore have unwind tables and are not `nounwind` (LLVM; Cranelift always emits unwind
tables). In a program that doesn't catch Velt panics (`catchUnwind`) they contain no landing pads:
nothing in an unwound frame is dropped. In one that does, a call in a frame that owns values has a
cleanup pad (vir.rs invariant 14; docs/internals/design/unwinding.md "Cleanup in unwound frames"),
and the frame's personality lands there only for Velt's own exception: this Rust unwind is a foreign
class to it and passes those frames in both phases, so their pads don't run and what the frames own
leaks. Every runtime function an unwind can pass through is `extern "C-unwind"`: `velt_rt_panic`,
the task ABI (§1, §2: `PollFn`, `DropFn` and the functions that poll promises) and the futures
behind it. The others stay `extern "C"`; a callback they run (drop, transfer, trace glue) runs no
user code.

A promise catches when `catchPanic` marked it (below) or when it was started, or is polled, while a
catching poll runs up the stack. A panic stopped at its poll marks it panicked: it counts as
finished, its result slot is never read, transferred or dropped, its state is leaked, and the node
is never freed. Its owner's await raises the panic again (unwinding further or ending the process),
so a panic travels through `await`s as through calls. Every path that reads a finished node's result
slot checks for it: a shared promise's holders raise it (§1.2; a holder that drives a lazy original
catches it for all of them), combinators that poll it raise it, and its result is never dropped,
transferred or handed to `mark_handled`'s quiet drop. A catching poll also records the thread's
runtime-level holds (the `Mutex` count and how the module lock is held, §9) and puts them back when
it catches, releasing a module lock taken for a `with`; the program's own `Mutex` stays locked.
WebAssembly does not unwind: there a panic always ends the program (velt_rt_wasm defines the
functions below as no-ops).

| Function | Signature | Semantics |
|---|---|---|
| `velt_rt_catch_arm` | `() -> void` | WebAssembly only (velt_rt_wasm) [velt_rt's removed, #2196: native programs catch panics on the executor in Velt, std/task/catch_sched.velt]. The next promise started on this thread (`velt_rt_fut_start*`) catches panics |
| `velt_rt_catch_take` | `() -> u64` | WebAssembly only, as `velt_rt_catch_arm`. The promise that took the arming, as a counted handle, or 0; disarms |
| `velt_rt_catch_settled` | `(u64 h) -> VeltFut*` | WebAssembly only, as `velt_rt_catch_arm`. Future of `bool`: waits until `h` finished, without claiming its result; true when it panicked |
| `velt_rt_catch_message` | `(u64 h, VeltStr* out) -> void` | WebAssembly only, as `velt_rt_catch_arm`. The panic message of `h` (empty if it did not panic) |
| `velt_rt_catch_free` | `(u64 h) -> void` | WebAssembly only, as `velt_rt_catch_arm`. Releases `h` (0 is ignored) |

std's `catchPanic(body)` (std/task/panic.velt, through `#task-catch`: std/task/catch_rt.velt
over these functions, std/task/catch_sched.velt on the executor in Velt) arms, calls `body` and
stores its promise (which starts it and takes the arming before its first poll; the first
promise started after arming takes it), takes the handle, awaits `settled`, and throws
`PanicError(message)` or awaits the promise. Arming and taking happen in one poll with no
`await` between them.

## 2. Tasks

A native program runs on the executor in Velt (std/lang/hooks/sched*.velt): lowering calls its
hooks (`sched_block_on`, `sched_spawn`, `sched_spawn_fut`, `sched_yield`, `sched_finish`) in place
of this section's symbols. The symbols below are the ABI WebAssembly programs call
(`velt_rt_wasm`); velt_rt defines them too, running tasks on its host (below), which also runs
the runtime's Rust glue (child processes on Windows, file streams).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_block_on` | `(PollFn poll, void* state)` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). `async main`, and a synchronous `main` of a program that starts promises or tasks (§1.1; its root calls `main`): `velt_main` builds the state on its stack, calls this, then reads the result at `state+0`. Runs the root as a task on the runtime's workers; returns when READY. Tasks still running afterwards are abandoned when the process exits. Must not be called from inside a task. |
| `velt_rt_tla_at` | `(const VeltStr* site)` | a top-level `await` (std/prelude/process.velt `topLevelAwaitAt`) at `site` (`path:line:col`, a string literal) suspends: called on the suspending path only (an `await` that goes on at once calls nothing), two atomic stores of the literal's bytes, no lock, no allocation. When the root can never finish, the site stored last makes the program write `Warning: Detected unsettled top-level await at <path>:<line>` and exit with 13, as Node does; with none stored it exits with 0 (`top_level_await.rs`, both runtimes) |
| `velt_rt_spawn` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). `spawn(f(...))`: copies the initial state into the task (caller gives up ownership of its contents), starts it now. Returns the join handle; its result slot (+16) receives `result_size` bytes (≤ 256; box larger results). Dropping the handle **detaches** (task keeps running); a result the handle never claims (dropped before or after the task finished) is dropped with `result_drop` (null: nothing to drop). |
| `velt_rt_spawn_transfer` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot), void (*result_transfer)(void* slot)) -> VeltFut*` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). `velt_rt_spawn` for a result that can reach counted objects: `result_transfer` (compiled transfer glue, in place) runs on the result as the task's state finishes, on the task and inside its local set (so promises the task started, which may still use the result's objects, are on the same thread, and a promise the glue starts joins the set), before the join handle can see it. Null: as `velt_rt_spawn`. |
| `velt_rt_spawn_detached` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align)` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). spawn whose result is unused: no handle, one allocation |
| `velt_rt_keep_alive_acquire` | `()` | WebAssembly only (velt_rt_wasm); on native targets lowering calls the executor in Velt's hook (std/lang/hooks/sched_end.velt `keepAcquire`). Takes a keep-alive reference: a ref'd timer is pending (std/prelude/timers.velt). |
| `velt_rt_keep_alive_release` | `()` | WebAssembly only (velt_rt_wasm), as `velt_rt_keep_alive_acquire`. Releases one reference taken with `velt_rt_keep_alive_acquire`; the caller keeps them balanced. `velt_rt_block_on` returns only once none is held. |
| `velt_rt_timer_unref_due` | `() -> VeltFut*` (result `bool`) | WebAssembly only (velt_rt_wasm): ready at once with false, as its one thread runs the timers and decides the end. On native targets lowering calls the executor in Velt's hook (std/lang/hooks/sched_end.velt `unrefDue`): a due `unref()`'d timer asks whether to drop its callback, true once the process was seen with nothing left that can happen, as Node exits without running it (std/prelude/timers.velt). |
| `velt_rt_spawn_fut` | `(VeltFut* f, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). `spawn(p)` where `p` is already a heap future (boxed promise, leaf); takes ownership of `f`; `result_drop` as for `velt_rt_spawn` |
| `velt_rt_spawn_target` | `(i64 worker)` | WebAssembly only (velt_rt_wasm); native programs run on the executor in Velt (#2196). `spawn(p, { worker })` (#211): the next spawn on this thread pins its task to `worker`. Called right after the task's arguments are evaluated, immediately before `velt_rt_spawn` / `_transfer` / `_detached` / `_fut`. Out of range (`worker < 0` or `≥ velt_rt_worker_count()`): panic `spawn: worker <w> is out of range (the runtime has <n> workers)`. In range, velt_rt spawns the task as usual (its host has one worker). |
| `velt_rt_worker_count` | `() -> u64` | the configured workers, `VELT_THREADS` (a positive integer) or one per core, fixed for the process (1 on WebAssembly): `workerCount()` on WebAssembly (std/task/worker_index.velt; on native targets the executor in Velt answers), and std/net's listener worker count (std/net/rt.velt). |
| `velt_rt_current_worker` | `() -> u64` | an index for this thread, always below `velt_rt_worker_count()`: `currentWorker()` on WebAssembly. |
| `velt_rt_set_scheduler` | `(u32 kind)` | `1`: what `velt_main` calls first when the program pins a task (`spawn(p, { worker })`); a no-op. `2`: each thread of the executor in Velt calls it before it polls its first task (std/lang/hooks/sched_workers.velt, spare workers too): the thread enters the host's tokio context for the rest of the process, so the leaves it polls find the host's timers and I/O, and the runtime takes the process's tasks for the executor's (they have no runtime task). |

velt_rt's host (`task/runtime/host.rs`) is one tokio multi-thread runtime with one worker
thread (`velt-host`, an 8 MiB stack) and tokio's blocking pool, started on first use. Child
processes on Windows, file streams, signals, the `velt dev` host, the native-package ABI and
velt_rt's own definitions of the symbols above run there. Everything in the runtime goes
through `task/runtime/` (`spawn`, `spawn_blocking`, `enter`, `block_on`, its `JoinHandle` /
`AbortHandle`); nothing else names tokio's. Neither the compiler nor the runtime reads
`VELT_SCHEDULER`.
Programs run on the executor in Velt (std/lang/hooks/sched*.velt), with `VELT_THREADS` workers
(a positive integer) or one per core. With more than one worker it rescues a stalled one
(std/lang/hooks/sched_monitor.velt); with one, only when `VELT_RESCUE=1`: no monitor thread, and a
poll that runs on holds back the rest, as in Node. `VELT_RESCUE=0` turns the rescue off, for
counting instructions with `VELT_THREADS=1` (`bench/nightly.sh`). `VELT_RESCUE_STATS=1`
makes the program print the rescues, spares started and lock hand-overs to stderr at its end
(std/lang/hooks/sched_stats.velt).
Panics inside tasks print `panic: <msg>` and exit 101 (same hook as sync code).

### 2.1 Latches (`new Promise`)

A one-shot latch (`u64` handle, §3.2): the waking half of `new Promise`
(std/lang/promise/settle.velt keeps the settled value in Velt code). Thread-safe; no callbacks are
stored (§13.5).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_latch_new` | `() -> u64` | a closed latch |
| `velt_rt_latch_free` | `(u64 latch)` | releases the handle (a pending wait keeps the latch alive) |
| `velt_rt_latch_open` | `(u64 latch)` | opens it and resumes every waiter; a no-op when open. A waiter of the task being polled (its root or one of its started promises) is queued in that task's microtask queue now, as Node queues a promise's reactions inside `resolve`; that entry is the waiter's `await` step. Under the executor in Velt, a latch's one waiter that is a started promise of that task, at rest, is not queued but run on at once: the function of a `new Promise` takes its settlement and finishes, so the promise is settled when `velt_rt_latch_open` returns, as in Node, the reactions of those who await it are queued then in the order they started waiting, and whoever awaits it later finds it settled; a function awaiting the promise as its owner stops at its `await`'s step, queued at the back in the entry's place (sched_latch.velt `settleNow`, #3775). Other waiters are woken. |
| `velt_rt_latch_wait` | `(u64 latch) -> VeltFut*` | completes (unit result) once the latch is open, and for a waiter queued by `velt_rt_latch_open`, once its queue reached that entry |
| `velt_rt_task_id` | `() -> u64` | WebAssembly only (velt_rt_wasm) [velt_rt's export removed, #2196: native code and std get the id from the executor in Velt, std/lang/hooks/sched_id.velt]. a unique id of the task being polled, never reused and kept by its local promises when they outlive it (0 outside a task; natively, a C library's thread has one for each outermost exported function's call, #2907; always 1 on single-threaded wasm): `resolve` copies a value settled from another task |

### 2.2 Channels (`velt/sync`)

`Channel<T>` (std/sync/channel.velt) is std code with no runtime functions. A channel is a
handle struct around a `shared` core whose block is raw memory from `velt_rt_alloc` (the
std-only intrinsics `rawAlloc`/`rawFree`): the items in a ring, moved in and out with
`slotStore`/`slotTake` under a spin lock (a sent value is transferred like a `spawn` argument by
`slotTransfer` before the lock is taken, so a deep copy never runs under it), and the tasks
waiting in two intrusive lines (receives, sends). A wait is a heap future
(§1) std makes (`fnAddress` for its poll and drop): it holds a clone of its poll's waker (§1's
C waker, on every executor), joins and leaves its line in constant time, and is woken after the
lock is let go. Values still queued when the last copy of a channel is dropped are dropped with
it. Process-wide objects std needs (velt/net's websocket topics, the `fetch` client's HTTP/2
sessions) are kept in static atomics (`Atomic<shared<T> | null>` module constants), released at
exit for the leak counters.

### 2.3 Abort signals (`velt/task`)

Each signal's `u64` handle is owned by a private `shared` cell in std/task/signal.velt and released
once, at the cell's last reference; no handle is public. Aborting sets a flag, stores the reason and
wakes the waiters; it never cancels anything itself. No code pointers are stored.
`AbortSignal.timeout`'s timer is std's, one of Node's timers (`velt_rt_timer_*`,
std/task/signal_timeout.velt): its promise holds a reference to the signal until the delay is up and
aborts it with a message carrying the delay, and std passes `timed` to a wait for it. A signal made
by `any` holds references to its sources until it is aborted (they reach it only through links it
owns).

Natively the signals are std's (`#task-signal`: std/task/signal_sched.velt over signal_core.velt,
in raw memory; a wait is a heap future linked into its signal, woken in the order the waits
began). WebAssembly's runtime keeps its own:

| Symbol (velt_rt_wasm) | Signature | Notes |
|---|---|---|
| `velt_rt_signal_new` | `() -> u64` | WebAssembly only. A signal that is not aborted |
| `velt_rt_signal_retain` | `(u64 s) -> u64` | WebAssembly only. Another reference to it (a scope's child task holds one) |
| `velt_rt_signal_free` | `(u64 s)` | WebAssembly only. Releases a handle |
| `velt_rt_signal_abort` | `(u64 s, const VeltStr* reason)` | WebAssembly only. Aborts it and the signals derived from it (`any`); a no-op if aborted |
| `velt_rt_signal_aborted` | `(u64 s) -> bool` | WebAssembly only |
| `velt_rt_signal_reason` | `(u64 s, VeltStr* out)` | WebAssembly only. The reason (`""` while not aborted) |
| `velt_rt_signal_wait` | `(u64 s, bool timed) -> VeltFut*` | WebAssembly only. Completes (unit) once aborted; `timed` (a timer may abort it): the wait keeps the process alive |
| `velt_rt_signal_any` | `(const VeltArray<u64>* signals) -> u64` | WebAssembly only. Aborted with the first of `signals` that is (its reason); keeps them alive until then |

### 2.4 Task groups (`taskScope`)

The live-children count of a `taskScope` is std code on every target (std/task/group.velt: an
atomic word and a channel the last child closes); the runtime has no functions for it.

### 2.5 Turns a JS host drives (wasm32-unknown-unknown)

A JS host's thread belongs to its event loop, so nothing waits on it (velt_rt_wasm
`host_turn.rs`), and nothing unwinds through the module: every call from the host returns
normally, with the executor's state on the heap. On `wasm32-unknown-unknown`, `velt_main` does not
call `velt_rt_block_on`: it hands `main`'s root task to the runtime and returns.
On both wasm targets `cx` is the same `VeltCx` with 32-bit pointers (the word at offset 0, the
waker at 4; a header's table at its offset 0); velt_rt_wasm compiles velt_rt's `task/waker.rs`.

- `velt_rt_host_main(PollFn poll, const void* state, u64 size, u64 align, int32_t (*finish)(void*
  state))`: moves the root's unpolled state (`size` bytes, `align`) to the heap and runs the first
  turn. `finish` is the code `velt_main` runs after `velt_rt_block_on` on other targets (the
  uncaught error, the exit code), which the runtime calls with the heap state once the root is
  done; then it frees the state.
- `void velt_turn(void)` (export): a turn: run the microtasks, the ready tasks and the due timers
  until none is left, as `velt_rt_block_on` does between its sleeps, so Velt's own order is the
  native one; then ask for a wake-up at the earliest timer (every turn asks again: a host may
  call early, as hosts round delays). The end follows native programs (velt_rt `entry.rs`
  `finish`): once the root is done and the microtasks it queued ran, the runtime runs `finish`
  at once (an uncaught error is reported then, whatever is still pending). A code other than 0
  ends the program; with 0 the turns go on until no task `spawn` started and no started promise
  is running and no keep-alive reference is held. When no timer or unsettled host promise can
  wake a task any more, the program ends too (exit code 13 when the root waits at a top-level
  `await` nothing can settle). Then the import `velt.done(code)`. No-op once the program ended.
  WASI and the unit tests' `velt_rt_block_on` return to `velt_main` at the same point, and the
  runtime runs the tasks on after a `main` that returned 0, sleeping between turns.
- `void velt_settle(u32 id)` (export): host promise `id` settled: wake the task its leaf
  registered (none if the leaf is gone), then `velt_turn()`.
- `velt_start` (export) runs `velt_main`; a `main` that hands no root over ends at once, with
  `velt.done(code)`.

Imports from module `velt` (the glue, crates/velt_rt_wasm/js/velt_web.mjs): `set_timer(f64 ms)`
(call `velt_turn` in `ms` milliseconds, replacing the previous request; a negative `ms` cancels
it), `done(i32 code)` (the program ended), `host_take(u32 id, void* out)` and
`host_forget(u32 id)` (§1.4). `velt.exit(code)` throws (`process.exit`, a panic): the
instance is done then. velt_link exports `velt_turn`, `velt_settle` and `__stack_pointer` (so a
host can check that turns leave it where it was) from every browser module.
A host that keeps the instance after `main` (a Worker) holds the program open with a started
promise or a keep-alive reference while it calls entries of its own that spawn tasks, and drives
them with the same `velt_turn` / `velt_settle`.

### 2.6 Transfers (std's transfer map)

The copies made during one transfer of a value to another thread are std's, not the runtime's
(std/lang/hooks/transfer.velt, #3584; velt_common `std_hooks` `xfer_*`, lowered from velt_vir
`lower/transfer_map.rs`): a per-thread map from each counted object copied to its copy, so an
object reached twice is copied once and a cycle is copied as a cycle (#351, #352). Generated code
calls the hooks as it calls any std function. Sema loads the module only for a program whose live
code can start a transfer (velt_common `Construct::Transfer`: a `spawn`, sema's `Transfer` of a
stored value, `slotPut` / `slotTransfer`, `shared(x)`, a lock's callback, an async closure that
copies its captures); in any other no transfer is ever open, and lowering answers the calls below
other than the begins as outside a transfer (`xferFind` 1, `xferKeeps` 0, `xferDefer` 0,
`xferSuspend` 0).

| Hook | Signature | Notes |
|---|---|---|
| `xferBegin` / `xferEnd` | `()` | bracket one transfer of a value to another task (the `TransferRoot` glue, a `thread_copy`); nest. The outermost `xferEnd` releases the deferred references (`xferDefer`) and forgets the copies |
| `xferFind` | `(usize obj) -> usize` | the copy of the counted object `obj` made in this transfer, 0 if none yet, or `1` outside a transfer. The clone glue of a counted object asks it before copying one whose count is above 1, the transfer glue before copying a still-shared one |
| `xferRecord` | `(usize obj, usize copy)` | `copy` is the copy of `obj` in this transfer, and of itself (a graph just copied and transferred next is not copied again); no-op outside one |
| `xferSuspend` / `xferResume` | `() -> u32` / `(u32 depth)` | set the transfer under way aside while user code runs (a class's own `clone()`), and continue it: in between nothing is looked up or recorded in it, and a transfer the user code starts gets a map of its own |
| `xferDefer` | `(usize obj, usize drop) -> u8` | the transfer gave up its reference to `obj` (replaced by a copy): `1`, and `drop` (the type's drop glue, `void (*)(void* slot)`) runs on it at the outermost `xferEnd`, so every object in the map stays alive with its count until then; `0` outside a transfer (the caller releases it) |
| `xferKeepBegin` | `()` | `xferBegin` of a value a `read` callback keeps in its caller's containers (§9.1): when it is the outermost one, the transfer glue keeps the lock-set objects the reader did not make (the caller's) with one more reference instead of copying them |
| `xferShareBegin` | `()` | `xferBegin` of a value sema transferred out of a `read` callback into a container of the code outside every read (§9.1): when it is the outermost one, the transfer glue keeps an object the reader made and still references (`LOCAL`, count above 1) instead of copying it, through `velt_rt_lk_share_made`, and transfers its parts in turn; a copy it makes of what the reader shares with the value is transferred in turn, so it is no longer `LOCAL` either |
| `xferKeeps` | `() -> u8` | 1 inside a transfer `xferKeepBegin` started, 2 inside one `xferShareBegin` started, else 0 |

The runtime never calls the map: generated code asks `xferKeeps` and passes the answer to read
mode's `velt_rt_lk_leave` (§9.1). std's catch (std/task/unwind.velt `caught`) closes what a panic
left open with `xferSuspend`, `xferResume` and `xferEnd`; the runtime's own catch (`velt_rt`
`panic.rs`) is armed only by `#task-catch`'s runtime variant, which no native build takes and
which is a stub on WebAssembly. The map's blocks are the C library's, outside the leak
counters; a thread keeps its transfer's block, with room for 1,024 entries, for the next one.

## 3. Results and errors

```c
typedef struct { int32_t code; uint32_t _pad; VeltStr message; } VeltErr;   // 32 bytes
// IoResult<T> = { VeltErr err; T value; }  -- value at offset 32 (T align <= 8)
```
- `code == 0`: success, `value` initialized (moved to the caller), `message` is empty static.
- `code != 0`: failure, `value` bytes are zero and must not be read or dropped; `message` is an
  owned string (drop with `velt_rt_str_drop`).
- Sync functions write an `IoResult` (or a bare `VeltErr` status) to a trailing out-pointer and
  **return nothing** (§3.1); callers read `err.code` from the slot.
- `IoError.code` (a string in Velt): std's `errorCodeName` (std/native/error.velt) names a code.

| code | name | code | name |
|---|---|---|---|
| 1 | `ENOENT` | 9 | `EADDRINUSE` |
| 2 | `EACCES` | 10 | `EPIPE` |
| 3 | `EEXIST` | 11 | `EOF` |
| 4 | `EINVAL` | 12 | `ENOTSUP` |
| 5 | `EILSEQ` (not UTF-8) | 13 | `ENOTDIR` |
| 6 | `ETIMEDOUT` | 14 | `ENOTEMPTY` |
| 7 | `ECONNREFUSED` | 15 | `EISDIR` |
| 8 | `ECONNRESET` | 16 | `EBADF` (closed handle, §3.2) |
|  |  | 99 | `UNKNOWN` (other) |

### 3.1 Extern declarations must match exactly

`std/*.velt` binds runtime functions with `declare function`, which the compiler lowers like a
Velt function: scalars (`bool` = `u8`, `number` = `f64`, the sized integers) stay scalars; every
other value (string, array, struct, tuple, closure) is passed as a pointer to it; `Promise<T>`
is a `VeltFut*`; a non-scalar result becomes a **trailing out-pointer and a `void` return**. The
runtime's definitions must be exactly that signature: native linkers tolerate an ignored return
value or a `u64` where a 64-bit pointer is expected, but WebAssembly links only identical
signatures (wasm32 pointers are 32 bits). `crates/velt_rt/tests/std_externs.rs` compares every
declaration in `std/` with velt_rt's and velt_rt_wasm's definitions.

`declare host("m") function` (https://veltlang.com/docs/reference/host-functions/) is not this ABI
either: it is a WebAssembly import the JS glue provides, called with the signature velt_vir's
`vir/host.rs` states (rt_abi.md "Host values").

`declare c("lib") function` (docs/internals/design/c-ffi.md) is not this ABI either: it binds a C
library function, not the runtime, called with the target's C convention as the declaration
types it (velt_vir `lower/c_call.rs`). After a call of one declared with `{ errno: true }`, the
`int` that `__error()` (macOS), `__errno_location()` (Linux) or `_errno()` (Windows' C runtime;
`GetLastError()` for another Windows library) gives is stored into the thread-local value
`velt_tls_*_32_0` of std's `std/os/rt::OS_ERROR:I32` (crate::tls, velt_vir `c_abi::ERRNO_SLOT`),
which `lastOsError()` reads (`OS_ERROR.get()`). std's file-descriptor functions are such
declarations (std/fs/fd/posix.velt), with the guards a C call needs in Velt beside them.

### 3.2 Object handles

Runtime objects that Velt code holds (`VeltListener`, `VeltStream`, `VeltServer`, `VeltReq`,
`VeltResp`, `VeltChild`, `VeltFileReader`,
`VeltFileWriter`, `VeltJson`) are **`uint64_t` handles**, 0 = none. Velt has no
pointer type, so `std` stores them in `u64` fields and passes `u64` arguments; the runtime takes
and returns the same `u64`, in argument lists, results and result slots alike.
- Handles that `std` wraps in **Copy structs** (`VeltListener`, `VeltStream`, `VeltChild`,
  `VeltFileReader`, `VeltFileWriter`) are keys into runtime handle tables
  (`velt_rt::registry::Key<T>`: generation, shard and slot), because Velt code may hold several
  copies: releasing one (`close`/`free`) makes every copy dead. Later operations fail with `EBADF`
  (code 16, "handle is closed"), sync accessors return their documented empty value (port 0, pid 0,
  exit code -1, empty address), and releasing again is a no-op; a stale handle never reaches a newer
  object. In-flight operations hold their own `Arc`, so the object lives until they finish.
- The others are the object's address (`velt_rt::handle::Handle<T>`, `repr(transparent)`): an
  `Arc` (JSON nodes) owned by a class whose `dispose()` releases it exactly once. std
  keeps every handle in a `private` field (the standard library may use the private members of
  its own types), so user code can neither build nor read one.

## 4. Byte buffers and string arrays

```c
typedef struct { uint8_t* ptr; uint64_t len; uint64_t cap; } VeltBytes;     // a u8[] (Vec<u8>)
typedef struct { VeltStr* ptr; uint64_t len; uint64_t cap; } VeltStrArray;  // Vec<VeltStr>
```
Heap buffers come from the rt allocator (`velt_rt_alloc(cap, 1)` / `(cap*24, 8)`), so generated
code may adopt them as `u8[]` / `string[]` storage. `VeltBytes` is not a `VeltStr` (strings have
their own forms); functions taking one never accept the other.

| Symbol | Signature |
|---|---|
| `velt_rt_bytes_drop` | `(VeltBytes* b)` — frees if `cap > 0`, zeroes. Never given an array whose `cap` has bit 63 set (an owner, `rt_abi.md` "Array length limit") |
| `velt_rt_bytes_from_str` | `(const VeltStr* s, VeltBytes* out)` — copy |
| `velt_rt_bytes_to_str` | `(const VeltBytes* b, IoResult<VeltStr>* out)` — copy, `EILSEQ` if invalid |
| `velt_rt_str_array_drop` | `(VeltStrArray* a)` — drops elements + buffer, zeroes |

## 5. std/fs

Arguments are copied at the call (the caller keeps ownership). Paths are UTF-8 strings. Data
arguments are strings (`VeltStr`).

```c
typedef struct { uint64_t size; double mtime_ms; uint8_t is_file; uint8_t is_dir;
                 uint8_t is_symlink; } VeltStat;                 // 24 bytes; is_symlink: lstat only
typedef struct { VeltStrArray names; VeltBytes kinds; } VeltDirents;  // 48 bytes
```

`VeltDirents` holds the sorted names and, at the same index, each entry's kind: 0 other (FIFO,
socket, device), 1 file, 2 directory, 3 symlink (not followed). The kind comes with the listing
(`d_type`, or Windows' find data); only an entry of unknown type costs an `lstat`.

velt_rt defines these sync functions: a native build runs them on the executor's blocking pool
(std/fs/rt_sched.velt). Each has an async twin of the same name without `_sync`, taking the
same arguments without `out` and returning a `VeltFut*` of the same result. Only velt_rt_wasm
defines the twins (WebAssembly builds, std/fs/rt.velt), so they are not in this table, whose
names the JIT host exports (crates/velt_rt_host/tests/abi_symbols.rs).

Reading, writing and appending whole files, `stat`, `lstat` and `exists` are std code in a
native build (`#fs-whole`: std/fs/whole/posix.velt over the C library's descriptors,
`#fd-system`; whole/win32.velt over `CreateFileW`, `ReadFile`, `WriteFile` and
`GetFileInformationByHandle`), with Node's messages, and text decoded with an invalid sequence
as U+FFFD, as Node decodes it. velt_rt has no functions for them, so the table has none.
velt_rt_wasm has, for WebAssembly builds, with their async twins:
`velt_rt_fs_read_file_bytes_sync(path, out)` (`IoResult<VeltBytes>`),
`velt_rt_fs_write_file_sync(path, data, out)` and `velt_rt_fs_append_file_sync(path, data, out)`
(`IoResult<()>`; append creates the file), `velt_rt_fs_stat_sync(path, out)` and
`velt_rt_fs_lstat_sync(path, out)` (`IoResult<VeltStat>`) and `velt_rt_fs_exists_sync(path) ->
u8`. Text is read as bytes and decoded in std: neither runtime reads a file as text.

Listing a directory (plain and with each entry's type), making one and removing a file, a link
or a tree are std code in a native build too (`#fs-tree`: std/fs/tree/posix.velt over the C
library's `opendir`, `readdir`, `mkdir`, `rmdir` and, for a tree, `openat`, `fdopendir` and
`unlinkat`; tree/win32.velt over `FindFirstFileExW` and `CreateDirectoryW`, and
tree/win_remove.velt, which lists a directory from its handle, opens each entry relative to it
with ntdll's `NtCreateFile` and deletes it on its handle), with Node's messages and the names
sorted by code point. On every system a tree is removed without reaching anything through a
link, also one that takes a directory's place during the removal. velt_rt has none of these.
velt_rt_wasm has, for WebAssembly builds, with their async twins:
`velt_rt_fs_read_dir_sync(path, out)` (`IoResult<VeltStrArray>`, the names sorted),
`velt_rt_fs_read_dir_typed_sync(path, out)` (`IoResult<VeltDirents>`),
`velt_rt_fs_mkdir_sync(path, recursive, out)` and `velt_rt_fs_remove_sync(path, recursive, out)`
(`IoResult<()>`).

Renaming, copying and the links (`rename`, `copyFile`, `symlink`, `readlink`, `realpath`) are
std code in a native build too (`#fs-paths`: std/fs/paths/posix.velt over the C library's
`rename`, `symlink`, `readlink` and `realpath`, with `copyFile` done as libuv's
`uv_fs_copyfile`, `st_size` bytes of data: a read and write loop on macOS (paths/darwin.velt;
`ENOTSUP` for a source of another kind, as libuv's `sendfile` there) and `copy_file_range` or
the loop on Linux (paths/linux.velt); paths/win32.velt over
`MoveFileExW`, `CopyFileW`, `CreateSymbolicLinkW`, the reparse point `DeviceIoControl` reads
and `GetFinalPathNameByHandleW`, as libuv uses them), with Node's messages. WebAssembly builds
have them as std code as well (std/fs/rt.velt calls `#fs-paths`, the async forms running the
sync code): under WASI paths/wasi.velt over wasi-libc's `rename`, `symlink`, `readlink`,
`realpath` and, for `copyFile`, `open`, `fstat`, `ftruncate`, `read`, `write` and `unlink`
(libuv's steps without permissions or clones), WASI's errno numbers read as Linux's
(std/ffi/errno_wasi.velt); under a JS host paths/js.velt, where each fails with `ENOTSUP`.
Neither runtime has functions for them.

All `path`/`from`/`to`/`data` parameters are `const VeltStr*`. Error messages are Node's: `<CODE>:
<description>, <syscall> '<path>'` (plus ` -> '<to>'` for `rename`/`copyfile`/`symlink`), e.g.
`ENOENT: no such file or directory, lstat 'x'` from `fs_remove`; the file streams'
`open_read`/`open_write` (§14.7) use the same form. A failed read or write of an opened file names
no path (`EISDIR: illegal operation on a directory, read`), and a directory opened as a file is
`EISDIR` on every system (Windows reports access denied).

## 6. std/net (TCP)

Native programs' TCP sockets are std's, on the executor's reactor (std/net/tcp_sched.velt); TLS
is Velt's over them (§14.8). velt_rt keeps one function here, `velt_rt_tcp_listen`, for the
listener `velt dev`'s supervisor hands over (§13.1). The stream functions below are WebAssembly
only: velt_rt_wasm defines them without a network (crates/velt_rt_wasm/src/net.rs), so that
programs that reach `fetch` link and run, and std/net/rt.velt declares them for that build.
Connecting fails with `ENOTSUP` (`fetch failed: WebAssembly programs have no network access`),
so no stream ever exists: an operation on one fails with `EBADF` (`handle is closed`), as for a
closed handle.

| Symbol | Signature | Result slot / notes |
|---|---|---|
| `velt_rt_tcp_listen` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<u64>`: the listening socket (a non-blocking descriptor, or a `SOCKET` on Windows), which the caller owns; `addr` = `"host:port"`, port 0 = any free port. Only std/net/tcp_sched.velt calls it, under `velt dev` (§13.1), and registers the socket with the executor's reactor; listeners are std's. Outside `velt dev` it fails (unsupported): the runtime binds nothing itself |
| `velt_rt_tcp_connect` | `(const VeltStr* addr) -> VeltFut*` | WebAssembly only. `IoResult<VeltStream>`: ready at once with `ENOTSUP` |
| `velt_rt_tcp_try_read_into` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end, IoResult<i64>* out)` | WebAssembly only. `readInto`'s probe (`data` is a `u8[]` the caller lends for the call; sema's written-parameter list has it): writes `EBADF` to `out` |
| `velt_rt_tcp_readable` | `(VeltStream s) -> VeltFut*` | WebAssembly only. `IoStatus`: ready at once with `EBADF` |
| `velt_rt_tcp_readable_within` | `(VeltStream s, i64 ms, u64 signal) -> VeltFut*` (`IoResult<u8>`) | WebAssembly only. ready at once with `EBADF` |
| `velt_rt_tcp_write_range` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end) -> VeltFut*` | WebAssembly only. `IoResult<()>`: ready at once with `EBADF` |
| `velt_rt_tcp_try_write_range` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end) -> u64` | WebAssembly only. 0 (nothing written) |
| `velt_rt_tcp_abort` | `(VeltStream s)` | WebAssembly only. does nothing |
| `velt_rt_tcp_close` | `(VeltStream s)` | WebAssembly only. does nothing |
| `velt_rt_tcp_park` | `(VeltStream s, const VeltStr* key)` | WebAssembly only. does nothing (`fetch`'s keep-alive pool) |
| `velt_rt_tcp_unpark` | `(const VeltStr* key, u32 settled) -> VeltStream` | WebAssembly only. 0 (no stream parked) |

## 7. HTTP (no runtime functions)

The HTTP client (std/net/http/fetch/client) and server (packages/http-server) are written in Velt
on std/net's sockets (§6; TLS in Velt, §14.8): the runtime has no HTTP of its own. Under `velt dev` a server
joins the graceful stop through velt/dev (std/dev, §13.2), which has no runtime functions.

## 8. Process

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_process_args` | `(VeltStrArray* out)` | UTF-8 (wide APIs on Windows), argv[0] included |
| `velt_rt_process_node_argv` | `(VeltStrArray* out)` | Node's `process.argv`: `[current executable, script, ...args]`; the script is `$VELT_SCRIPT` (read and removed at start-up; `velt run` sets it) or the JIT host's `set_script`, else the executable. wasm: `[program, program, ...args]` (WASI: the module path; the browser passes none, so `["", ""]`) |
| `velt_rt_exit` | see rt_abi.md | |

`process.memoryUsage()` is std code on native targets (`#memory-sys`: std/process/
memory_linux.velt reads `/proc/self/statm`, its resident pages and, for `heapUsed`, those no file
backs; memory_darwin.velt `task_info(TASK_VM_INFO)`'s `resident_size` and `phys_footprint`;
memory_win32.velt kernel32's `K32GetProcessMemoryInfo`, `WorkingSetSize` and `PagefileUsage`).
`velt_rt_memory_rss() -> i64` and `velt_rt_memory_heap() -> i64` are the WebAssembly runtime's
only: both the linear memory's size (std/process/memory_wasm.velt).

Whether a file descriptor is a terminal (Node's `tty.isatty`) is std code on native targets
(`#tty`: std/process/tty_posix.velt over `isatty`, tty_win32.velt over `GetConsoleMode`, each
cached for 0, 1 and 2); `velt_rt_isatty(i32 fd) -> u8` is the WebAssembly runtime's only
(std/process/tty_wasm.velt).

The environment and the working directory are std code on native targets: `#env-sys`
(std/process/env_posix.velt over `getenv`/`setenv`/`unsetenv` and `environ`, under one std lock;
env_win32.velt over Win32's wide functions) and `#cwd-sys` (cwd_posix.velt over
`getcwd`/`chdir`, cwd_win32.velt). The WebAssembly runtime keeps its own (std/process/env_wasm.velt
and cwd_wasm.velt; WASI's environment, empty in the browser): `velt_rt_env_get(const VeltStr*
name, VeltStr* out) -> u8` (0 = unset), `velt_rt_env_set(const VeltStr* name, const VeltStr*
value)`, `velt_rt_env_remove(const VeltStr* name)`, `velt_rt_env_all(VeltStrArray* out)`
(`[name0, value0, …]`), `velt_rt_process_cwd(IoResult<VeltStr>* out)` and
`velt_rt_process_chdir(const VeltStr* path, VeltErr* out)`.

`Date.now()` and `performance.now()` are std hooks (`dateNow`, `perfNow`,
std/lang/hooks/clock.velt over `#clock`: `clock_gettime`, the performance counter and
`GetSystemTimePreciseAsFileTime` on Windows, WASI's `clock_time_get`, a JS host's `velt:clock`),
which lowering calls; `velt_main` stamps `performance.now()`'s time origin first (`clockStart`).
Neither runtime has clock functions.

## 9. Shared state and helpers (sync)

`shared<i64>`'s `add`, `get` and `set` have no runtime functions: lowering emits them inline as
`velt_atomic_*` externs (rt_abi.md "Atomics").

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_rc_inc` | `(u64* rc)` | `shared.clone()`; Relaxed (like `Arc`); past `MAX_REFCOUNT` it stops the program (`panic: reference count overflow`, exit 101) |
| `velt_rt_rc_dec` | `(u64* rc) -> u8` | Release; returns 1 (after an Acquire fence) when it reached zero: caller drops + frees |
| `velt_rt_thread_yield` | `()` | `velt/sync`'s `Backoff.snooze` once spinning stops: let another thread run (`sched_yield`); nothing on wasm |
| `velt_rt_mutex_init` | `(u64* lock)` | the `Mutex<T>` lock word: 8 bytes, align 8, inside the shared object; no destroy needed |
| `velt_rt_mutex_lock` / `velt_rt_mutex_unlock` | `(u64* lock)` | WebAssembly only (velt_rt_wasm): `m.with(f)` = lock; call f; unlock. On a native target the lock and unlock are std's (std/lang/hooks/mutex.velt): a compare-and-swap of the word's first 32 bits (bit 0 held, bit 1 threads parked), and the two slow paths below. Blocks the thread; bodies must not await. |
| `velt_rt_mutex_lock_slow` / `velt_rt_mutex_unlock_slow` | `(u64* lock)` | native: std's lock found the word held (spin, then park until it is free or handed over), or its unlock found threads parked (wake one, or hand the lock to it) |
| `velt_rt_mutex_lock_checked` | `(u64* lock)` | the lock from code that may run in read mode (§9.1): there it stops the program before locking; its unlock is std's (native) or `velt_rt_mutex_unlock` |
| `velt_rt_rwlock_init` | `(u64* lock)` | the `RwLock<T>` lock words: 19 words (152 bytes), align 8, at the start of the `RwLock<T>`, then the value (std `prelude/sync.velt`; §9.2) |
| `velt_rt_rwlock_read` | `(u64* lock, u64 reader) -> u64 how` | `rw.read(f)`: the lock shared with other readers; the thread is in read mode (§9.1) until the unlock, which takes `how` back. `reader` is `how & ~7` of an earlier `read` or `write` in the same call of the calling function (never across an `await`: the task may continue on another thread), else 0 |
| `velt_rt_rwlock_read_unlock` | `(u64* lock, u64 how)` | the end of the read that returned `how`, on the same thread (callbacks never await) |
| `velt_rt_rwlock_write` | `(u64* lock, u64 reader) -> u64` | `rw.write(f)`: the lock exclusively; in read mode it stops the program before locking (§9.1). `reader` as for `read`; the result is this thread's reader state, for the unlock and later calls |
| `velt_rt_rwlock_write_unlock` | `(u64* lock, u64 reader)` | the end of the `write` that returned `reader` |
| `velt_rt_lk_mode` | `() -> u8` | 1 when this thread is in read mode (a thread-local read, §9.1) |
| `velt_rt_lk_leave` | `(u64* count, void* obj, void (*drop)(void** slot), u8 keeps)` | the reader's own lock-set object `obj`, whose count word is at `count`, is transferred out of a `read` callback: clears `LOCAL` unless the frame it goes to still runs in read mode, that frame being `velt_rt_lk_leave_to`'s number of callbacks out (1 by default: a nested `read`'s result stays the outer reader's). `keeps` is std's `xferKeeps` for the transfer under way (§2.6), which the caller asks. One the readers still reference (its count above 1) that a nested `read` stores into code outside every read, or that a `read` stores into the caller's containers (`keeps` 1: a transfer std's `xferKeepBegin` started), keeps `LOCAL` until the thread's outermost read ends (the callbacks may still change it), with one more reference meanwhile, which `drop` (its drop glue) releases once the bit is cleared; transferred again before then, at any depth, it stays so (its count never drops to 1, so a transfer to another thread copies it) (§9.1) |
| `velt_rt_lk_keep_made` | `(u64* count)` | a transfer started by std's `xferKeepBegin` moves the reader's own object whose count word is at `count`, which the reader may still reference: going to a frame more `read` callbacks out than `velt_rt_lk_leave_to` says, its count becomes 1 (the caller's reference) without `LOCAL`; otherwise, and for an object `velt_rt_lk_leave` already left the readers' own, the count stays, and `velt_rt_lk_leave` decides (§9.1) |
| `velt_rt_lk_share_made` | `(u64* count, void* obj, void (*drop)(void** slot))` | a transfer started by std's `xferShareBegin` keeps the reader's own object `obj`, whose count word is at `count`, which the reader still references: going to a frame still in read mode (`velt_rt_lk_leave_to`) it stays that reader's own, counted; going to code outside every read it gets `STORED` next to `LOCAL`, so a change through the readers' references stops the program (`velt_rt_lk_write`), and holds one more reference until this thread's outermost read ends, when both bits go and `drop` (its drop glue) releases that reference; one a store already left the readers' own (`velt_rt_lk_leave`) stays so (§9.1) |
| `velt_rt_lk_kept_again` | `(void* obj) -> u8` | a transfer started by std's `xferKeepBegin` keeps the object `obj`, which the readers still reference, as itself: 1 when a store in this thread's reads kept it already (its parts were kept then and are not counted again), else 0, and it is recorded until the outermost read ends (§9.1) |
| `velt_rt_lk_kept_before` | `(void* obj) -> u8` | 1 when a store in this thread's reads already kept the object `obj` as itself (`velt_rt_lk_kept_again`): moved on again (its count back at 1), its parts are not kept, and the caller's objects among them not counted, a second time (§9.1) |
| `velt_rt_xfer_keep_shared` | `(u8 enter)` | such a transfer goes into (`enter` 1) or comes out of (0) the parts of an object the readers still reference, kept as itself: `velt_rt_lk_leave` keeps those parts the readers' own until the reads end, also one referenced only there, since the readers may still change them through it (§9.1) |
| `velt_rt_lk_leave_to` | `(u64 levels) -> u64` | the transfers that follow move into a frame `levels` `read` callbacks out from the running one (`u64::MAX`: code outside every read; `0`: the running callback itself, so the object stays the reader's own and keeps `LOCAL`, for a copy lent to a method of the lock's value or of a temporary the reader made), for a store into a container a nested callback captured; returns the number it replaces, which the caller puts back after them (§9.1) |
| `velt_rt_deep_eq_enter` | `(u64 a, u64 b) -> u8` | the cycle guard of `deepEqual` and `toEqual` (std/lang/equal's generated code) and of `==` on aggregates holding types that can hold themselves (#1881), by the objects' identities (addresses): 1 if this thread is comparing `a` with `b` already (equal), 2 if it is comparing `a` or `b` with another object (different), else 0 and the pair is being compared until `velt_rt_deep_eq_leave` |
| `velt_rt_deep_eq_leave` | `() -> ()` | the pair `velt_rt_deep_eq_enter` last took on is compared |
| `velt_rt_lk_write` | `(u64* count)` | a write of the lock-set object whose count word is at `count`: stops the program in read mode unless the reader made the object (§9.1) |
| `velt_rt_free_after_reads` | `(void* obj, void (*drop)(void** slot))` | the last reference to the lock-set object `obj` (count still 1) went in code changing the caller's containers inside a read: `drop` (its drop glue) frees it now, or in read mode once this thread's outermost read ends (§9.1) |
| `velt_rt_str_hash` | `(const VeltStr* s) -> u64` | string Map/Set keys; fixed seed (deterministic), not flood-resistant |
| `velt_rt_math_sqrt` / `_floor` / `_ceil` / `_round` / `_trunc` / `_fabs` | `(f64) -> f64` | `round` = JS `Math.round` (ties toward +∞, keeps `-0`) |
| `velt_rt_math_to_int32` | `(f64) -> i32` | JS ToInt32 (bitwise operators on numbers); backends emit the case of magnitude below 2^63 inline and call it only for the rest |
| `velt_rt_math_mul_int32` | `(i32 a, i32 b) -> i32` | JS `(a * b) \| 0`: the exact product rounded like the double multiply, low 32 bits; emitted inline |
| `velt_rt_math_add_int32` | `(i32 a, f64 x) -> i32` | JS `(a + x) \| 0`; emitted inline, integer add when `x` is a whole number of at most 2^52 |
| `velt_rt_math_clz32` | `(i32) -> i32` | `Math.clz32` of the 32-bit pattern (32 for 0); emitted as one instruction |
| `velt_rt_math_clz64` / `_ctz64` / `_popcount64` | `(u64) -> u64` | `x.leadingZeros()` (64 for 0), `x.trailingZeros()` (64 for 0) and `x.countOnes()` of a 64-bit pattern (std/prelude/bits.velt); emitted as one instruction |

### 9.1 Read mode (`RwLock.read`)

Readers of an `RwLock<T>` run on many threads at once without copies or atomic counts. The
*lock set* is the counted types a `T` of some `RwLock` can reach (velt_vir `boxing/locks.rs`;
an `RwLock` whose `T` reaches a function or interface value, a class in a hierarchy, a weak-capable
or proxiable type, or a promise has no such readers: its `read` takes the lock exclusively).
Between `velt_rt_rwlock_read` and its unlock the thread is in *read mode*, and the code it runs
counts and writes lock-set objects by these rules, inline (velt_vir `rc.rs`):
- bit 62 of a lock-set object's count word, `LOCAL`, marks an object made in read mode: only this
  reader reaches it. A count compares and decrements without the bit. A transfer that moves one
  out of the callback (its result) clears it;
- a reader counts (retains, releases, tests for uniqueness) only `LOCAL` objects; the others (the
  store) it borrows, so their counts stay what the last writer left. No writer runs meanwhile;
- a reader writes only `LOCAL` objects: anything else calls `velt_rt_lk_write`, which stops the
  program (sema rejects the writes it can see);
- a reader takes no lock exclusively: `velt_rt_rwlock_write` and `velt_rt_mutex_lock_checked`
  (which code not known to run outside read mode calls instead of a plain lock) stop the
  program in read mode, since the inner callback could not tell the store's uncounted objects
  from what it changes (sema rejects the ones written in a `read` callback's body);
- a reader starts no promise or task (`velt_rt_fut_start*`, `velt_rt_spawn*` stop the program in
  read mode once the program has made an `RwLock`): what it holds would outlive the callback
  without leaving through its result;
- what the callback returns leaves by these rules too (velt_vir `async_fn/sync.rs`), before the
  unlock, whatever the calling code's mode: an object it shares with the store is copied, never
  moved out;
- the caller's containers (places rooted in a variable a `read` callback literal captured from code
  outside read mode, velt_vir `Work::ReadOwn`, also through each `read` callback around a nested
  one, `leave_levels.rs`; that code is also a function instance counting by the counts outside read
  mode, `Work::Deferred`, which a reader calls to change the caller's containers, and code whose
  mode is asked when the thread is outside read mode as the literal is evaluated) hold no store
  object and are counted by the counts outside read mode. What a store, a `push` or a method or
  function changing one (velt_vir `LkMode::Deferred`) keeps there gets a reference by those counts
  when it is a place the caller holds already (also a reader's variable borrowing one; what comes
  from the store sema copies), so it stays the same object; anything else is a copy of what the
  reader did not make. A value sema transferred into such a container (velt_vir leave_levels.rs
  `share_dest`) stays itself when the reader made it, by a transfer std's `xferShareBegin`
  starts: it becomes the caller's, its parts are transferred in turn (what the reader shares with
  the value becomes a copy), and the reader's other references borrow it uncounted. A value the
  reader made that holds what the caller holds is kept by a transfer std's `xferKeepBegin`
  starts: the caller's objects get one more reference and stay themselves, and what the reader made
  becomes the caller's. A container of the reader's that such a method or function may change is
  counted by those counts for the call and handed back after it, or, when its type is outside the
  lock set and holds lock-set objects (a class's `Item[]`, a `Map`), passed as a copy of what the
  reader did not make, dropped by those counts after the call. What leaves one (an element removed,
  the old value a store replaces, such a method's or function's result) loses its reference by those
  counts, and a lock-set object whose last reference goes is handed to `velt_rt_free_after_reads`
  with its drop glue: the reader may still point at it uncounted, so it is freed when the thread's
  outermost read ends (at once outside read mode; the glue releases a reference, so one stored again
  meanwhile lives on). A result the reader keeps is borrowed uncounted from then on, as the store's
  objects are, unless the reader made it (`LOCAL`).

Code whose mode is fixed counts without asking: functions only called directly from outside read
mode (and `with` / `write` callbacks, which never run in read mode) count by the counts outside read
mode; functions only called directly from a `read` callback literal use read mode's rules.
Everything else (callbacks given as values, closures that may outlive the call, polls, glue in
tables) asks `velt_rt_lk_mode` once per call.

### 9.2 How `RwLock` readers go in

The lock is *biased* (Dice and Kogan, "BRAVO: Biased Locking for Reader-Writer Locks", 2019) until
it is written: a reader names the lock in a slot of its own thread (a cache line no other thread
writes, velt_rt `hazard.rs`) and checks that no writer came, so readers on many threads write
nothing they share. A writer *revokes* the bias: it sets the lock's state and waits for the slots
naming the lock to clear, then takes the lock's underlying reader-writer lock (parking_lot's
`RawRwLock`) exclusively. While it waits the state says so (revoking): one writer revokes at a time,
and another writer, holding the underlying lock or not, writes only once the state says the bias is
revoked, and waits for the revocation without holding the lock. Each change of the state has one
owner (biased to revoking: the revoker's compare-exchange; revoking to revoked: that revoker;
revoked to biased: a reader's compare-exchange), so a revoker held up for any time never takes a
later revocation for its own. It never waits for the slots while holding the underlying lock: a
reader in a slot may be nested in a read of another lock whose writer waits for this writer's
readers. A reader that biased the lock again before the writer took it (its revocation was old
enough) makes the writer let the underlying lock go and revoke once more. From then on readers take
the underlying lock shared, so they still run at once and a lock written often costs about what a
`Mutex` does, until a reader holding it finds the revocation old enough and biases the lock again. A
writer blocked on the underlying lock holds new outermost readers back, so writers don't starve,
while a read nested in another goes past it (a recursive shared lock), so readers nesting reads of
two locks in opposite orders never wait for each other's writers. How long a revocation lasts adapts
to how long the bias before it lasted (at most 10 ms). The lock words are the underlying lock, then
(on the next 128-byte line, which only revocations write) the state.

A thread waiting for the underlying lock backs off exponentially between tries (about 0.5 µs
doubling to 35 µs, yielding each time, blocking after about 0.7 ms): a waiter that tries takes
the line from the holder, so short callbacks taking turns run several in a row on one core.
Generated code passes each lock call the reader state an earlier one in the same call of the
function returned, so the common reads and writes run without a
thread-local lookup or a call.

## 10. Output with many tasks

`console.log` from tasks uses per-thread stdout buffers; the runtime publishes a thread's buffer
into the shared stdout buffer after every task poll, so output order follows causality (`log a;
await x; log b` prints a before b even if the task moved threads) and lines from different tasks
never interleave mid-line (a full thread buffer is published only through its last newline; only a
single line longer than 1 MiB may be split). Buffered output reaches the OS when buffers fill, when
a worker goes idle (so servers logging to a pipe show output promptly), before stderr writes, on
`velt_rt_flush`, and at exit.

## 11. Worked example (what lowering emits)

As emitted for WASI (a JS host's `velt_main` hands the root to `velt_rt_host_main`, §2.5). On a
native target the calls of `velt_rt_sleep`, `velt_rt_spawn` and `velt_rt_block_on` are the
executor's hooks `sched_sleep`, `sched_spawn` (the same arguments and the result's transfer glue,
null here) and `sched_block_on` (the same arguments and whether tasks must stay on their worker).

```c
// async function add1After(a: i64, ms: i64): Promise<i64> { await sleep(ms); return a + 1; }
typedef struct { int64_t result; uint32_t tag; int64_t a, ms; VeltFut* sleep; } Add1After;
uint32_t add1After$poll(void* s, void* cx) {
    Add1After* st = s;
    switch (st->tag) {
    case 0: st->sleep = velt_rt_sleep(st->ms); st->tag = 1; /* fallthrough */
    case 1: if (!st->sleep->poll(st->sleep, cx)) return 0;
            st->sleep->drop(st->sleep);
            st->result = st->a + 1; st->tag = 2; return 1;
    }
}
void add1After$drop(void* s) { Add1After* st = s; if (st->tag == 1) st->sleep->drop(st->sleep); }

// const h = spawn(add1After(41, 5)); ... await h
Add1After init = { 0, 0, 41, 5, 0 };
st->h = velt_rt_spawn(add1After$poll, add1After$drop, &init, sizeof init, 8, sizeof(int64_t), NULL);
... if (!st->h->poll(st->h, cx)) return 0;
    int64_t v = *(int64_t*)((char*)st->h + 16); st->h->drop(st->h);

// async function main() { ... }   →   int32_t velt_main(void) {
//     Main m = {0}; velt_rt_block_on(main$poll, &m); return 0; }
```

## 12. Strings & JSON

Implemented in `crates/velt_rt` (`strbuf.rs`, `str_ops/`, `json/`), tested from Rust against
node-generated tables in `crates/velt_rt/tests/abi/` and from C in
`crates/velt_rt/tests/link_check.rs`. Strings are `VeltStr`s in canonical WTF-8 (UTF-8 that may
hold lone surrogates; forms in rt_abi.md "Strings").

**Ownership conventions of this section.** Input strings are borrowed (`const VeltStr*`, caller
keeps ownership). Every `VeltStr* out` receives a string the caller owns and drops with
`velt_rt_str_drop` — but a result that is a sub-range of a **static/borrowed** input may itself
be borrowed from it (static form, same lifetime as the input, drop is a no-op), exactly as
`velt_rt_str_clone` keeps static strings static. A result equal to a whole heap input shares it
(count +1); other results are fresh (inline when ≤ 23 bytes).

### 12.1 String builder

```c
typedef VeltStr VeltStrBuf;   // same layout; any owned VeltStr is a valid builder
```
Template literals, `a + b + c` chains and generated `JSON.stringify`/print glue build with one
builder: O(total length), amortized doubling. A push appends in place to an inline builder with
room or a heap buffer with count 1; anything else (static text, a full inline string, a shared
buffer) first moves the text to a fresh buffer, so a push never changes another copy of the
string. `finish` moves the text out (a heap result of ≤ 23 bytes becomes inline, freeing the
buffer).

Appending to a variable or field (`s += x`, `s = s + x`, `` s = `${s}${x}` ``, where the old
value of `s` is dead after the assignment) uses the same in-place path on `s` itself: the text
after the leading `s` is pushed straight onto `s` when no part of it can throw, else built into
a builder of its own and appended with `velt_rt_str_append` (rt_abi.md), so a throwing part
leaves `s` unchanged. When the text may change `s` (it reads `s`, or calls code that could reach
it), lowering shares the old value before evaluating it and puts it back afterwards, which
leaves the count at 1 again unless the text kept a copy. Every push ends in the one append
routine of the runtime (`VeltStr::push_wtf8`, rt_abi.md "Strings"), which keeps the string's
UTF-16 unit count, lone-surrogate count and form up to date per append in O(1) (the appended
text's counts come from its value when it is a string, as in `velt_rt_str_append`).
Appended text lying in the target's own buffer (the target itself, a share, an uncounted copy or
a static-form view of it) is copied out before the buffer grows, moves behind a header or has a
surrogate pair joined at its end.

**Inline appends (#1677).** `JSON.stringify`'s glue appends ASCII literal text (up to 32 bytes)
without a call when it can: when bits 62 and 63 of `w2` are clear (a heap string, not a slice)
and the text fits (`(w1 as u32) + n <= w2`), it stores the bytes after the text and adds
`n * 0x1_0000_0001` to `w1`; otherwise it calls `velt_rt_strbuf_push_byte` / `_push_bytes`. It
does not check the buffer's count: that builder is fresh and no other code reaches it before
`finish`. So the heap form's `w1`/`w2` layout is part of this contract.

**Words appended inline (#1982).** std's intrinsic `strbufPushAsciiWords(buf, w0, w1, w2, len)`
(numbers as text, std/lang/number) appends up to 24 ASCII bytes held in three words the same
way, with room for all 24: when the builder is a plain heap string with
`(w1 as u32) + 24 <= w2`, it copies the 24 bytes after the text and adds `len * 0x1_0000_0001`
to `w1` (the bytes past the new end are spare capacity). An inline ASCII builder (byte 23 is
`0x80 | n`, its bytes past `n` are 0) with `n + len <= 23` gets the words shifted `n` bytes on
and OR'd into its own, and byte 23 becomes `0x80 | (n + len)`. Otherwise it calls
`velt_rt_strbuf_push_words` with the words by value. Like the literal appends, it does not check
the buffer's count, for the reason given there.

**Invariant:** a count-1 buffer is appended to or adopted (and so possibly reallocated, and for
an adoption its text moved within it) only while no borrowed static-form view into it is live.
The only such views are the JSON reader's borrowed keys (§12.3), which point into the
source text; generated decode glue never appends to the text it is reading, and a template
literal adopts only a call's result that it owns. Appended text may lie in the target's own
buffer (a share, an uncounted copy or a static-form view of it): it is copied out before the
buffer grows. An adopted string's head must not lie in its buffer.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_strbuf_new` | `(u64 cap, VeltStrBuf* out)` | `cap` = initial capacity hint (≤ 23: starts inline, else allocates up front) |
| `velt_rt_strbuf_adopt` | `(VeltStr* part, const VeltStr* head, u64 cap, VeltStrBuf* out) -> u8` | a builder holding `head + *part` in `part`'s own buffer, grown to `cap` (clamped like `new`'s hint): 1 when `part` is the only reference to a heap buffer of its own (not a slice, literal or inline string) whose layout fits the result, and `head` has no lone surrogate; `*out` is then the builder and `*part` empty. Otherwise 0 and nothing changes. `head` must not lie in `part`'s buffer. Template literals use it for a long fresh part |
| `velt_rt_strbuf_push_str` | `(VeltStrBuf* b, const VeltStr* s)` | `s` may be `b` itself or lie in its buffer |
| `velt_rt_strbuf_push_bytes` | `(VeltStrBuf* b, const u8* p, u64 len)` | static text chunks of a template; `len == 0` ⇒ `p` unused. The low 32 bits of `len` are the byte count; the high 32 bits may carry the UTF-16 unit count (as a string's `w1`): equal to the byte count, the text is ASCII and is not scanned; 0 means unknown (counted) |
| `velt_rt_strbuf_push_words` | `(VeltStrBuf* b, u64 w0, u64 w1, u64 w2, u64 n)` | the first `n` (at most 24) bytes of the three words, ASCII, byte `i` in bits `8 * (i % 8)` of word `i / 8`: the slow path of `strbufPushAsciiWords`, by value so the caller needs no stack for the words (#1982) |
| `velt_rt_strbuf_push_u8s` | `(VeltStrBuf* b, const VeltBytes* bytes, u64 len, u64 units)` | the first `len` bytes of a `u8[]` (canonical WTF-8 Velt code built, e.g. std/encoding/json/writer.velt); `units == len`: ASCII, not scanned; otherwise counted |
| `velt_rt_strbuf_push_bool` | `(VeltStrBuf* b, u8 v)` | `true`/`false` |
| `velt_rt_strbuf_push_byte` | `(VeltStrBuf* b, u8 c)` | punctuation in generated glue |
| `velt_rt_strbuf_finish` | `(VeltStrBuf* b, VeltStr* out)` | moves the text to `*out`; `*b` becomes empty (reusable, nothing to free) |
| `velt_rt_strbuf_commit` | `(VeltStrBuf* b, u64 n, u8 ascii)` | the `n` bytes written after the text, in the builder's spare room (a unique heap buffer), become part of it: UTF-8, ASCII when `ascii` is 1 (not counted then) |
| `velt_rt_strbuf_drop` | `(VeltStrBuf* b)` | abandon an unfinished builder (exception path); zeroes it |
| `velt_rt_rows_exact_i64` | `(f64 v, i64* out) -> u8` | 1 and the integer when `v` is one exactly (an integer field from a float cell of a `RowSource`), else 0 |

### 12.2 String methods

**Indexing model**: indexes and lengths are **UTF-16 code
units**, as in JavaScript (they agree with `s.length`). An ASCII string works on its bytes, whose
offsets are its positions; another string works on its WTF-8 bytes and translates positions at
entry and exit (rt_abi.md "Breadcrumbs"). A position may fall between the two halves of a pair:
`slice` there re-encodes the half as a lone surrogate (`"😀".slice(0, 1)` is `"\uD83D"`), and a
needle that starts with a lone low surrogate or ends with a lone high one also matches half of a
pair (`"😀".indexOf("\uDE00")` is 1), searched in code units. `split("")` and
`replaceAll("", x)` work per code unit, splitting pairs. A string may hold lone surrogates
(rt_abi.md "Strings"): the pieces of a built result join a high surrogate meeting a low one into
the pair; case mapping keeps lone surrogates; number parsing and `localeCompare` read each as
U+FFFD. "Omitted" JS arguments are passed as the value given in Notes.

The searches, `split`, `trim` / `trimStart` / `trimEnd`, `replace` / `replaceAll`, `repeat`,
`padStart` / `padEnd`, the code points of `for (const c of s)` and the case mapping of ASCII text
are Velt code (std/lang/string/, std/lang/hooks/string.velt; #2198), with these rules; the
runtime keeps the functions below.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_byte_slice` | `(const VeltStr* s, u64 start, u64 end, u64 units, VeltStr* out)` | **[non-additive, #2816]** bytes `start..end` of the WTF-8 (velt/encoding `utf8Substring`, `trim`, `split`, and `slice` / `substring` / `charAt` / `at`, whose positions std translates); `units` is the piece's UTF-16 length, or `u64::MAX` when the caller doesn't know it (the runtime counts); the caller checks `start <= end <= len` and code point boundaries (`slice` itself is std's, the runtime's JS slice is gone) |
| `velt_rt_str_eq` | `(const VeltStr* a, const VeltStr* b) -> u8` | `==` fast path: length check + one memcmp |
| `velt_rt_str_join` | `(const VeltStrArray* parts, const VeltStr* sep, VeltStr* out)` | `parts.join(sep)`: sums the pieces' lengths and unit counts, then writes the result once (inline, or a heap buffer of exactly its size) |
| `velt_rt_str_is_well_formed` | `(const VeltStr* s) -> u8` | 1 when `s` has no lone surrogates (std's `encodeURIComponent`) |
| `velt_rt_str_to_upper` / `_to_lower` | `(const VeltStr* s, VeltStr* out)` | full Unicode default case mapping (`ß` → `SS`, final sigma), like JS, for text that is not ASCII; ASCII text is mapped in std (std/lang/string/case.velt), which calls these for the rest |
| `velt_rt_str_from_char_code` | `(i64 code, VeltStr* out)` | JS `ToUint16(code)`; a surrogate gives a lone surrogate, as JS. std makes the string of an ASCII unit itself (inline, no call) and calls this for the others |
| `velt_rt_str_to_number` | `(const VeltStr* s) -> f64` | exact JS `Number(s)`: trimmed; `""` ⇒ 0; `0x`/`0o`/`0b` (unsigned); `±Infinity`; whole string must be a decimal literal, else NaN |
| `velt_rt_str_array_drop` | `(VeltStrArray* a)` | already in §4 |

### 12.3 JSON pull reader (`JSON.parse<T>`)

The reader is std's Velt code, with no runtime functions: `Reader`
(std/encoding/json/reader.velt), which the decoders the compiler generates per target type drive
over JSON text. JSON is strict RFC 8259 unless the options ask for JSONC; the first failure is
sticky.

### 12.4 JSON error messages

| Situation | Message |
|---|---|
| well-formed, but a different kind than the decoder asked for (incl. non-integral / out-of-range for `read_i64`), or no reader error at all (e.g. missing field) | `expected <expected> at <path>` — e.g. `expected string at $.name` (golden) |
| the text is not JSON (anywhere in it, also past the point where the decoder stopped; `JSON.parseValue` too) | Node 22's `JSON.parse` message, e.g. `Expected property name or '}' in JSON at position 1 (line 1 column 2)`, `Unexpected end of JSON input`, `Unexpected token 'x', "[x]" is not valid JSON` (positions, lines and columns in UTF-16 units) |
| syntax error the reader sees that V8 accepts (none is known) | `invalid JSON at <path>: <detail> (byte <offset>)`; for `parseValue` the path is that of the value being read when the error occurred (`$[1]`, `$.a`) |

The reader checks the whole text for a Node syntax error first (only on this failure path, so it
costs nothing on success), then gives the decoder's message. Over a driver's rows only the first
row and the unknown-key messages occur (a nested JSON cell's own failure is a mismatch at its
path).

With the reader's options: an unknown key (rejected) is `unknown field at <path>` (the decoder
puts the key in the path), and nesting past `max_depth` is
`JSON nested deeper than <max_depth> levels at <path> (byte <offset>)` (also from
`JSON.parseValue`). The prelude passes `max_depth` 128 unless the program sets
`maxDepth`.

`<detail>` is one of `unexpected end of input`, `unexpected character 'c'` (control characters as
`U+XXXX`), `expected ':'`, `expected ',' or '}'`, `expected ',' or ']'`, `expected string key`,
`invalid number`, `invalid escape`, `invalid \u escape`, `control character in string`,
`unexpected trailing characters`. `<offset>` is the byte offset in the source's UTF-8 (not a
string position: it locates the error in the input text as an editor or a byte-level tool sees
it; for ASCII input the two agree). Path syntax
(`$`, `$.a.b`, `$.tags[1]`) is the compiler's choice; the reader inserts it, shortened when it
has more than 20 segments (each starting at `.` or `[`) to the first and last 10 with `…`
between (`$[0][0]…[0].name`).

### 12.5 `json.Value` (`JSON.parseValue`)

A `JsonValue` is std's Velt code (std/prelude/json/json_value.velt: a tape, or a tree of nodes
built and edited in place, docs/internals/design/json-engine.md); it has no runtime functions.

### 12.6 Performance

`cargo test -p velt_rt --release --lib text_perf -- --nocapture` prints the builder's time for
1M `push_str` + 1M `push_byte` into one string. The reader's speed is `bench/json/`'s.

## 13. Dev mode (`velt dev`, [hot-reload.md](../design/hot-reload.md))

Nothing here changes the ABI generated code uses; it is how the runtime behaves under the
`velt dev` supervisor, and how `velt` hosts JIT-compiled programs.

### 13.1 Listener handover
`VELT_DEV_SOCKET` names the supervisor's dev channel: a Unix socket path, or on Windows a local
named pipe (`\\.\pipe\velt-dev-<pid>-<n>`, byte mode; a program retries for up to 5 s while every
pipe instance is busy). Each request is one connection. When the variable is set,
`velt_rt_tcp_listen` connects, sends
`listen <addr>\n` (the address exactly as the program passed it) and receive the listening
socket, or `err <message>\n` (the operation then fails with that message):
- Unix: `ok\n` with the socket's descriptor attached (`SCM_RIGHTS`);
- Windows: `ok <hex>\n`, where `<hex>` is the `WSAPROTOCOL_INFOW` record (lowercase hex, two
  digits per byte) from `WSADuplicateSocketW` for the requesting process (the supervisor takes its
  id from the pipe, `GetNamedPipeClientProcessId`); the program opens its own descriptor of the
  same socket with `WSASocketW(FROM_PROTOCOL_INFO, …, WSA_FLAG_OVERLAPPED |
  WSA_FLAG_NO_HANDLE_INHERIT)`.

The supervisor
binds each address on its first request and returns the same socket to every later request and
program version, so connections queue in the kernel backlog across restarts and port 0 keeps
its port. Without the variable the runtime binds the address itself.

### 13.2 Stopping
In dev mode (`VELT_DEV_SOCKET` set) std handles a stop request while a server that joined the
graceful stop (velt/dev's `joinStop`) has not reported itself drained (`drained`;
std/dev/stop.velt): such servers stop accepting (the supervisor still holds the socket), in-flight
requests get up to 1 s to finish, stdout is flushed and the process exits with status 0. The
runtime has no functions for it. While no server has joined, or once every one drained, the stop
request keeps its default (the program is terminated).

The stop request is:
- Unix: SIGTERM. Outside dev mode SIGTERM keeps its default action.
- Windows (no SIGTERM): when a server joins, the program opens a **stop channel**, a
  connection that sends `watch-stop\n` and gets `ok\n` (closed again once every server drained); the supervisor keeps it (by the client
  process id) and writes `stop\n` to ask for the stop. The supervisor closing the channel (it
  exited) also counts as a stop request. A program without a stop channel (no server joined) is
  terminated, like SIGTERM's default action. A Unix supervisor answers `watch-stop` with `err`.

The supervisor kills a program that has not exited 1.5 s after the stop request.

### 13.3 Build report (JIT host)
One more request on the same dev channel, sent by `velt dev --host` (not by programs): `built ok\n`
or `built failed\n`, then one line per source file the build read, then an empty line. After `ok`
the host blocks until the supervisor answers `go\n` (sent once the previous version has stopped),
then runs the program.

**Reload channel** (hot swap, [hot-reload.md](../design/hot-reload.md)): after `go` the host keeps
that connection open. For each later version the supervisor writes `reload\n`; the host
builds the current sources while the program runs and answers `swapped <n>\n` (n functions
swapped into the running program), `restart <reason>\n` (the program must restart to run the
new version; nothing was changed) or `failed\n` (the build failed; the host printed the
diagnostics), each followed by one line per source file the build read and an empty line. A
closed channel means the other side has gone away.

### 13.4 Hosting JIT code in `velt` (`velt_rt_host`)
`crates/velt_rt_host` compiles `crates/velt_rt/src` a second time as an rlib for `velt`, with
`cfg(velt_rt_host)` removing the C `main` (a Cargo *feature* would not do: features unify across a
workspace build and would strip `main` from the staticlib every program links). Its
`[dependencies]` and `[features]` must equal `velt_rt`'s (`crates/velt_rt_host/tests/manifest.rs`).
It exposes:
- `abi_symbols::ABI_SYMBOLS: [(&str, AbiAddress); N]`, generated by `crates/velt_rt/build.rs` from
  every `#[no_mangle] extern "C"` or `extern "C-unwind"` function in the sources (except `main`);
  the JIT registers it with `JITBuilder::symbol`. A test checks it contains every `velt_rt_*` name
  in the tables of rt_abi.md and this file. Other symbols (`memcpy`, `fmod`, ...) resolve from the
  process.
- `entry::run_main(velt_main)`: what `main` does (runtime init, call, flush stdout) → exit code.
- `process::set_args(argv)`: `argv()` / `args()` for the hosted program (`velt`'s own arguments
  are not the program's).
- `process::set_script(path)`: the script of Node's `process.argv[1]` for the hosted program
  (`velt dev`); otherwise the runtime reads `$VELT_SCRIPT` once at start-up and removes it.

On Windows x64 the JIT (`velt_codegen_cl::DevSession`) registers each loaded program's unwind
info with `RtlAddFunctionTable` (code and records in one arena), so stack walks get through JIT
frames as they do through linked code's `.pdata`.

### 13.5 The rule against cached code pointers
A hot swap reaches new calls through each function's trampoline; a future in flight keeps the
`poll`/`drop` of the version that created it (its `VeltFut` header). Rule: only vtables (via
relocations) and `VeltFut` headers may store code addresses. Runtime APIs that take callbacks
(timers, WebSockets, child processes, ...) keep them replaceable the same way. The one exception is
drop glue kept with a value in flight (a join's or a spawned task's result): it matches the values'
layout, which a swap doesn't change (a changed layout restarts the program), and old code is never
freed, so the pointer stays callable after a swap. A value's drop glue goes away with the value.

## 14. Standard library breadth

Runtime symbols behind the std modules of https://veltlang.com/docs/std/. Same conventions as above:
string arguments are borrowed `const VeltStr*`, `out` parameters receive owned values, handles are
`u64`s (§3.2). None of them store code pointers (§13.5): callbacks in these modules are compiled
Velt code, never function pointers kept by the runtime.

`VeltArray<T>` = `{ T* ptr; u64 len; u64 cap; }` (cap in elements; read as `i64`, 0 and negative
values are memory the array does not own, `rt_abi.md` "Array length limit"), the layout of a Velt `T[]`
of a Copy element type, built from a Rust `Vec<T>` (same allocator), like `VeltStrArray`.

### 14.1 Randomness and local time

**[non-additive, #2196]** The operating system's random numbers are std code (`#os-random`:
std/lang/random/os.velt over `getrandom` on Linux, `getentropy` on macOS and under WASI,
`BCryptGenRandom` with the system's preferred generator on Windows, the JS host's
`crypto.getRandomValues`). velt/crypto's `randomBytes` and `randomInt`, UUIDs and the seed of
`Math.random()` draw through it; a failing generator stops the program (a panic).
`velt_rt_random_bytes` and `velt_rt_random_u64` no longer exist.

**[non-additive, #2196]** The local time zone's offset is std code (`#localtime`:
std/time/localtime_posix.velt over the C library's `localtime_r` and `tm_gmtoff`,
localtime_win32.velt over `TZ` when it names UTC or a fixed offset, else Win32's
`GetDynamicTimeZoneInformation` and `SystemTimeToTzSpecificLocalTimeEx`; UTC on WebAssembly):
`velt_rt_local_offset_minutes` no longer exists in either runtime.

### 14.2 Regular expressions (`velt/text`)

**[non-additive, #2196]** Regular expressions are std code
([the regular expression engine](../design/regex-engine.md), `std/text/regex/`): a pattern is
parsed, compiled and matched in Velt, on the subject's own bytes. The twelve `velt_rt_regex_*`
functions and the `VeltRegex` handle no longer exist in either runtime. std's `replace` builds its
result with the string builder (§12.1) and `velt_rt_str_byte_slice`.

### 14.3 Child processes (`velt/process/child`)

No runtime function: child processes are std's own. `execSync` blocks its thread without the
executor: std/process/child/exec_sync_posix.velt on macOS and Linux (`posix_spawn`, a `poll`
loop on its pipes and `waitpid`), std/process/child/exec_sync_win32.velt on Windows
(`CreateProcessW` with named pipes whose parent ends are overlapped, one
`WaitForMultipleObjects` loop on their reads and the input's write, then the process handle's
wait); on WebAssembly it fails with `ENOTSUP`, as `spawn` and `exec` do. `spawn` and `exec` are
std's own on every native target
(std/process/child/child_sched.velt: `posix_spawn` with the exit and the pipes on the executor's
reactor on macOS and Linux, `CreateProcessW` with the exit wait and the pipes on its blocking
pool on Windows). Exit codes: the process's code (std reports `128 + signal` for a child killed
by a signal on macOS and Linux). Spawn failures are `IoResult` errors whose message starts with
`spawn <program>: `.

### 14.4 Standard input (`velt/process`)

Standard input is std code: one process-wide read-ahead buffer
(std/process/stdin_buffer.velt) over `#stdin-sys` (`read` on fd 0 on macOS, Linux and WASI;
`ReadConsoleW` or `ReadFile` on Windows; none under a JS host), locked with a `Mutex`'s lock
(`#thread-lock`: std's on a native target, `velt_rt_mutex_lock` elsewhere);
an async read is the sync one on the executor's blocking pool (std/process/rt_sched.velt), and
text is decoded lossily. Neither runtime has stdin functions.

### 14.5 Machine facts (`velt/os`)

No runtime functions: `velt/os`'s facts are std code over the C library and Win32
(`#os-sys`: std/os/sys_darwin.velt, sys_linux.velt, sys_win32.velt; on WebAssembly
sys_wasm.velt), and its architecture is the build's `x64` or `arm64` condition (`#os-arch`).

### 14.6 UDP and DNS (`velt/net/udp`, `velt/net/dns`)

No runtime functions: `velt/net/udp`'s sockets are non-blocking sockets on the reactor of the
executor in Velt on every native OS (std/net/udp/udp_sched.velt; Windows' through AFD), and
`velt/net/dns`'s lookups run `getaddrinfo` on its blocking pool
(std/net/dns/lookup_sched.velt). On WebAssembly, std's default variants (std/net/udp/rt.velt,
std/net/dns/rt.velt) fail with `ENOTSUP`.

### 14.7 File streams (`velt/fs`)

File streams are std code in a native build: std/fs/stream_sched.velt keeps every open reader and
writer in one table for the process (a reader's file, its range left and its metadata as of the
open; a writer's file and its 64 KiB buffer), and runs each open, read, write and close on the
executor's blocking pool, over the C library's file descriptors on macOS and Linux and Win32
handles on Windows (`#fs-stream-os`). A handle used after `close` fails with `EBADF`; a writer
released unclosed is flushed then (errors ignored). Lines and text are std code
(std/fs/line_buffer.velt): a reader's bytes read ahead, 64 KiB per read, split at `\n` by
`indexOfByte` and decoded there. velt_rt has no file stream functions, and neither has
velt_rt_wasm: on WebAssembly every open fails with `ENOTSUP` (std/fs/stream_none.velt).

### 14.8 TLS

The runtime has no TLS (#2392). The TLS 1.3 and 1.2 client and server are Velt code
(`std/net/tls/`), over the reactor's sockets (a listener `velt dev` hands over is adopted onto the
reactor; an OS without a reactor gets `ENOTSUP`): `connectTls`, `listenTls`, `startTls`, the HTTP server, the
`fetch` client and WebSockets. Clients trust Mozilla's root certificates (a std table generated
from Mozilla's list) plus optional extra PEM CAs; no system trust store is read.

### 14.9 WebSockets (`velt/net/websocket`)

`velt/net/websocket` is written in Velt (`std/net/websocket/`) on std/net streams (§6): a client
connects with `connect` / `connectTls` (§14.8), a server takes the connection a
request on the Velt HTTP server switched to (`HttpResponse.upgrade`). A `WebSocket` is a struct of
the `TcpStream` and a `shared` value holding the connection's send queue and reader (one lock), so
there is no `VeltWs` handle and no `velt_rt_ws_*` symbol. It uses public APIs only (reads with
`TcpStream.read`, masking keys from `randomBytes`, text through `u8[]`'s `write` and `utf8Slice`),
except the process-wide registry of the topics, a static atomic (§2.2). A send encodes its frame
into the connection's queue at the call (call order is wire order, also before a server-side upgrade
completes) and completes once it is queued; a flusher task started by whoever queues into an idle
connection writes everything queued in one write (a reply by the connection's reading task, with no
more input waiting, is written at once instead), and a send waits while more than 1 MiB is queued.
No callbacks are stored (§13.5).

### 14.10 HTML escaping, databases and HTTP

These have no runtime functions. `escapeHtml` and std/jsx's escaping are Velt code
(`std/text/html.velt`). The SQLite, Redis and PostgreSQL clients are packages: `sqlite`
(`packages/sqlite`, which calls SQLite through `declare c`,
[c-ffi.md](../design/c-ffi.md)), `redis` (`packages/redis`, plain Velt on `velt/net`) and
`postgres` (`packages/postgres`, plain Velt on `velt/net` and its TLS client). The HTTP server
and client are Velt code (§7).

### 14.11 Fast random numbers

`Math.random()` and `velt/random` are Velt code (`std/lang/random/wyrand.velt`), seeded from the
operating system's random numbers (§14.1): under WASI wasi-libc's `getentropy`
([wasi-libc.md](../design/wasi-libc.md)), under a JS host the glue's `velt:random`.`fill`
(`crypto.getRandomValues`). No runtime function is involved on any target. Not for secrets.
