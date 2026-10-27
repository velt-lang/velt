# Runtime C ABI, part 2: async, std/fs, std/net, std/http, process, shared state, strings & JSON — PROPOSAL

Status: **proposal by the runtime agent, implemented in `crates/velt_rt`** (tested from Rust with
hand-written state machines in `crates/velt_rt/tests/abi/`, and from C in `tests/link_check.rs`).
It changes only through a reviewed contract change. Everything from `rt_abi.md` still holds (all pointers 64-bit, `bool` is
`u8`, `VeltStr` = static / inline / refcounted heap string, see rt_abi.md "Strings").
All functions are `extern "C"`, `#[no_mangle]`; VIR signatures are scalar/pointer only.

Semantics follow docs/reference/async.md: a stored promise **starts when it
is created** (`velt_rt_fut_start`, §1.1) and runs as a local promise of the task that created it;
directly awaited calls are embedded state machines; tasks run on a tokio multi-thread runtime.

## 1. Awaitable protocol

```c
typedef uint32_t (*PollFn)(void* state, void* cx);   // 0 = PENDING, 1 = READY
typedef void     (*DropFn)(void* state);

typedef struct VeltFut {                             // every runtime-owned future (16 bytes)
    uint32_t (*poll)(struct VeltFut* self, void* cx);
    void     (*drop)(struct VeltFut* self);
} VeltFut;                                           // result slot at byte offset 16
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
- `cx` points to the runtime's `{ const uint32_t* word; Context* cx; }`: the task's **await
  word**, then the Rust `&mut Context`. Generated code passes `cx` on and reads only the word
  (below, "Awaiting").
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
- *Heap future `f` (leaf, join handle, boxed promise):* `if (velt_rt_fut_poll(f, cx) == 0) return 0;`
  then move the result out of `(char*)f + 16` (type given per function below), then
  `velt_rt_fut_drop(f)` (frees it; never touches the result). `f->poll(f, cx)` may be called
  directly instead of `velt_rt_fut_poll`.
- *The step after an `await`* (user code; std code takes none, its prelude counts Node's steps
  with `velt_rt_microtask`): once the child or heap future is READY, the code after the `await`
  runs one microtask later, as in Node. `if (*cx->word != 0) { t = velt_rt_await_tick(cx); if
  (t != 0) { st->ticket = t; suspend; on resume: if (!velt_rt_await_ready(st->ticket, cx))
  return 0; } }`. The result stays where it is (child state offset 0, or moved out of the heap
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
| `velt_rt_fut_poll` | `(VeltFut* f, void* cx) -> u32` | `f->poll(f, cx)` |
| `velt_rt_fut_drop` | `(VeltFut* f)` | cancel if pending, free; never drops the result slot |
| `velt_rt_fut_box` | `(PollFn, DropFn, const void* state, u64 size, u64 align) -> VeltFut*` | moves a compiled initial state into a heap future (for `Promise<T>` values that are stored, put in arrays, returned); result at +16 = state offset 0. `align ≤ 16`. Lazy until polled or started (§1.1). |
| `velt_rt_fut_start` | `(VeltFut* f, void (*result_drop)(void* slot))` | start a boxed promise now (§1.1); no-op for any other future, or outside a task. `result_drop` disposes of an unclaimed result (null if nothing to do); for a promise that can reject it reports an `Err` like an unhandled rejection. |
| `velt_rt_fut_start_rejecting` | `(VeltFut* f, void (*result_drop)(void* slot))` | `velt_rt_fut_start` for a promise that can reject (its result is a `Result`, tag first; `result_drop` reports an `Err`): when it rejects while its owner does not wait for it, the rejection is reported as uncaught (through `result_drop`, which then gets 0 from `velt_rt_unhandled_defer`) once the task's microtask queue is drained, unless the owner awaited it, dropped it, or handed it to a combinator or another task by then, as Node reports a rejection without a handler. (additive) |
| `velt_rt_fut_transfer` | `(VeltFut* f, void (*transfer)(void* slot))` | the owner hands `f` to another task: `transfer` (compiled transfer glue, in place; null for a result with nothing counted, which only hands over a holder of a shared promise, §1.2) runs on the result slot on the task that produces the result, as it finishes (at once if a started promise already finished); with or without `transfer`, a started promise's rejection is no longer reported by the task that started it (`velt_rt_fut_start_rejecting`) but claimed or given up by the task holding it (#1497); a race passes it to its children; no-op for join handles (a task transfers its own result: `velt_rt_spawn_transfer`, or the mark of the promise `velt_rt_spawn_fut` runs) and runtime leaves. WebAssembly does the same on its one thread, so values are moved or copied where native targets move or copy them. |
| `velt_rt_xfer_begin` / `velt_rt_xfer_end` | `()` | bracket one transfer of a value to another task (the `TransferRoot` glue, a `thread_copy`); nest. The outermost `end` releases the deferred references (`xfer_defer`) and forgets the copies (additive) |
| `velt_rt_xfer_find` | `(const void* obj) -> void*` | the copy of the counted object `obj` made in this transfer, null if none yet, or `1` outside a transfer. The clone glue of a counted object asks it before copying one whose count is above 1, the transfer glue before copying a still-shared one, so an object reached twice is copied once and a cycle is copied as a cycle (additive) |
| `velt_rt_xfer_record` | `(const void* obj, const void* copy)` | `copy` is the copy of `obj` in this transfer, and of itself (a graph just copied and transferred next is not copied again); no-op outside one (additive) |
| `velt_rt_xfer_suspend` / `velt_rt_xfer_resume` | `() -> u32` / `(u32 depth)` | set the transfer under way aside while user code runs (a class's own `clone()`), and continue it: in between nothing is looked up or recorded in it, and a transfer the user code starts gets a map of its own (additive) |
| `velt_rt_xfer_defer` | `(void* obj, void (*drop)(void* slot)) -> u8` | the transfer gave up its reference to `obj` (replaced by a copy): `1`, and `drop` (the type's drop glue) runs on it at the outermost `end`, so every object in the map stays alive with its count until then; `0` outside a transfer (the caller releases it) (additive) |
| `velt_rt_xfer_keep_begin` | `()` | `velt_rt_xfer_begin` of a value a `read` callback keeps in its caller's containers (§9.1): when it is the outermost one, the transfer glue keeps the lock-set objects the reader did not make (the caller's) with one more reference instead of copying them [additive, #708] |
| `velt_rt_xfer_share_begin` | `()` | `velt_rt_xfer_begin` of a value sema transferred out of a `read` callback into a container of the code outside every read (§9.1): when it is the outermost one, the transfer glue keeps an object the reader made and still references (`LOCAL`, count above 1) instead of copying it, through `velt_rt_lk_share_made`, and transfers its parts in turn; a copy it makes of what the reader shares with the value is transferred in turn, so it is no longer `LOCAL` either [additive, #1541] |
| `velt_rt_xfer_keeps` | `() -> u8` | 1 inside a transfer `velt_rt_xfer_keep_begin` started, 2 inside one `velt_rt_xfer_share_begin` started, else 0 [additive, #708, #1541] |
| `velt_rt_drop_state` | `() -> u32*` | this thread's drop state word, which generated code updates in place around a drop that can nest (glue whose type leads back to itself, #543): bits 0..30 count the drops under way, bit 30 says something was queued, bit 31 that the queue is being drained. Per drop: `n = *w; if (n & 0x3fffffff) >= 128 { drop_queue(...) } else { *w = n + 1; <drop>; m = *w - 1; *w = m; if m == 1 << 30 { drop_drain() } }` (additive) |
| `velt_rt_drop_queue` | `(void* value, void (*drop)(void* value))` | drop `value` with `drop` (glue that takes it over: a closure env's drop, a field or struct value moved to a heap box, or a box with its last reference) when the outermost drop on this thread ends, and set the queued bit; at once while the thread is torn down (additive) |
| `velt_rt_drop_drain` | `()` | called by the outermost drop when it ends with the queued bit set: drops the queued values (which may queue more) in the order they were queued, then clears the word (additive) |
| `velt_rt_yield_now` | `(void* cx)` | `await yieldNow()` inline: call it, then `return 0`; resumes after other ready tasks. No allocation. |
| `velt_rt_yield_now_fut` | `() -> VeltFut*` | `yieldNow()` as a value; result: none |
| `velt_rt_await_tick` | `(void* cx) -> u64` | the step of an `await` when the word is not 0: 0 to go on now (nothing else can run first, or the waiter's step was queued by `velt_rt_latch_open` already), else the ticket of an entry queued at the back of the task's microtask queue for `cx`'s waker. No allocation. (additive) |
| `velt_rt_await_ready` | `(u64 ticket, void* cx) -> u32` | 1 once the queue reached `ticket` (or for a ticket of another queue); 0 while it waits, and the entry then resumes whoever polls with `cx` now. (additive) |
| `velt_rt_microtask` | `() -> VeltFut*` | waits for its turn in the microtask queue of the task that polls it (`then` / `catch` / `finally` callbacks and `queueMicrotask`, std/prelude/promise.vlt); result: none. The queue is first in, first out and is drained at every job boundary of the task: after the root's poll and after each local promise the task runs from its ready queue (entries queued meanwhile run in the same drain). An entry runs its waiter directly: a local promise of the task is run, the root is polled, any other waker is woken; an entry whose promise is driven by its owner by then is skipped (the owner's own entry resumes it). Outside a task it is ready at once. WebAssembly keeps one queue that runs before the ready tasks. `await` steps (`velt_rt_await_tick`) use the same queue. No code pointers are stored (§13.5). (additive) |
| `velt_rt_unhandled_defer` | `() -> u8` | called by the function that disposes of an unclaimed rejecting promise's result (`velt_rt_fut_start`'s `result_drop`) before it reports an `Err`: 1 when the runtime keeps the promise to report it once the task's microtask queue is drained, as Node does (the function then returns without reporting or dropping; the runtime calls it again then, and it gets 0); 0 to report now (outside a runtime drop of a started promise in a task). (additive) |
| `velt_rt_task_gate` | `() -> u64` | a counted handle to the gate of the task being polled (0 outside a task): a callback that runs on another task can wait with it for the end of its creator's poll (std/prelude/timers.vlt no longer needs it: its callbacks run on the creating task). Released with `velt_rt_task_gate_free`. WebAssembly: always 0. (additive) |
| `velt_rt_task_gate_wait` | `(u64 gate) -> VeltFut*` | completes once the poll the gate's task was in when the wait was first polled has ended (each poll ends with the task's microtask queue drained), at once when the task is not in a poll or for gate 0; so a timer's callback never runs before the callbacks its creator queued. Result: none. (additive) |
| `velt_rt_timer_new` | `(i64 ms) -> u64` | a place in the process-wide order of timer callbacks (`setTimeout`, `setInterval`, `setImmediate`; std/prelude/timers.vlt) for a timer of the task being polled, as Node orders timers: at the end of the task's list for delay `ms` (negative = 0); a new list's expiry is the monotonic clock read now in whole milliseconds plus `ms`. The lists of all tasks run by (expiry, creation of the list); a pass over the due timers reads the clock once and runs each due list's timers due by then in creation order, and a list whose next timer is not due by then waits for it with a new place (Node's `listOnTimeout`). `ms` 0 (or less) is an immediate (`setImmediate`): the task's immediates are not in a list; once a pass finds nothing more due, the immediates created by then run, in creation order, before the next pass reads the clock (Node's check phase). The turn waits at least `ms`. Insert, removal and finding the next timer take logarithmic time. Freed with `velt_rt_timer_free`. (additive) |
| `velt_rt_timer_turn` | `(u64 timer, u64 gate) -> VeltFut*` | completes once the timer is due, the poll `gate`'s task (0: none; the prelude passes 0, its callbacks run on the creating task) was in when it came due has ended (as `velt_rt_task_gate_wait`), it comes next in the order among the timers whose task is not busy in a poll of another task (a busy task's lists are passed over as a whole), and the poll of the task that took the previous turn has ended (each poll ends with its microtask queue drained); the timer then leaves the order. So callbacks run in Node's order, each followed by its microtasks, across tasks. Ready at once for a timer that is no longer queued (`velt_rt_timer_clear`). Called in the first run of a started promise (the prelude's), it marks that promise as a timer's: it does not keep an orphaned set alive (§1.1; a ref'd timer holds a keep-alive reference itself). WebAssembly: the same lists, fired by the executor, and `block_on` does not wait for such promises. Result: none. (additive) |
| `velt_rt_timer_again` | `(u64 timer, i64 ms)` | queues the timer again at the end of its list, due `ms` after the clock read when it last took its turn (an interval's next run, as Node's: called after the callback's synchronous part). (additive) |
| `velt_rt_timer_clear` | `(u64 timer)` | takes the timer out of the order (`clearTimeout`; a no-op once it took its turn or was cleared); its turn completes at once. May be called from any task while the handle is live (the prelude calls it under the timer state's lock, which also covers `velt_rt_timer_free`). (additive) |
| `velt_rt_timer_free` | `(u64 timer)` | takes the timer out of the order if it is still queued (its task ends without a turn) and frees the handle. (additive) |
| `velt_rt_task_gate_free` | `(u64 gate)` | releases a gate handle (0: no-op). (additive) |
| `velt_rt_sleep` | `(i64 ms) -> VeltFut*` | `sleep(ms)`; negative = 0; result: none |
| `velt_rt_all` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results) -> VeltFut*` | `Promise.all(array)`: takes ownership of the `n` futures (not of the pointer array); child `i`'s result is moved to `results + i*result_size` (must stay valid until completion/drop); concurrent, only woken children are re-polled. Result: none. |
| `velt_rt_all_with_drop` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | same as `velt_rt_all` (which = this with `result_drop == NULL`), for results that own resources: if the returned future is dropped **before completing**, `result_drop(results + i*result_size)` runs for every child `i` that had already finished (pending children are cancelled via their own drop). After completion it never runs — all results belong to the awaiter. Use it whenever `T` needs dropping. |
| `velt_rt_all_or_reject` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | `Promise.all` over promises that can reject: like `velt_rt_all_with_drop` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled), but completes as soon as a child rejects. Its result is empty. On a rejection the rejected child's result is moved to slot 0 and is the only initialized slot (so a rejection is an `Err` tag in slot 0, `n > 0`): the runtime drops the other finished results with `result_drop` (null: nothing to drop) and drops the pending children (started promises keep running; mark them with `velt_rt_futs_handled` first). |
| `velt_rt_race` | `(VeltFut* const* futs, u64 n, u64 result_size) -> VeltFut*` | `Promise.race(array)`: takes ownership of the `n` futures (not of the pointer array); the first child to finish moves its `result_size`-byte result to the returned future's slot (+16) and the others are dropped (started promises keep running, §1.1; one created during the task's current poll that is queued, woken by that poll's own code, runs once now, before the race's awaiter; so do the pending children of `velt_rt_all_or_reject` when it rejects). `n == 0` never completes. |
| `velt_rt_race_ok` | `(VeltFut* const* futs, u64 n, u64 result_size, void (*reject_drop)(void* slot)) -> VeltFut*` | `Promise.any`: like `velt_rt_race` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled): the first fulfilled child wins; a rejected one is dropped with `reject_drop` (null: nothing to drop) while others are still running, and the last rejection is the result when all reject. |
| `velt_rt_fut_detach` | `(VeltFut* f, void (*quiet_drop)(void* slot))` | the owner gives up `f` without cancelling it (pending siblings of an early `Promise.all` rejection). A boxed promise that is still lazy, even one its owner has polled, becomes a started promise of the current task. If it was created during the task's current poll (and nothing yielded during it), it is polled once now, before the owner's continuation: only that poll's own code can have woken it, as with a JS microtask queued before the rejection handler (#150). Otherwise it runs at the task's next poll, after the owner's continuation, as in JS, where the rejection handler runs before the timers and events that woke it. A started promise keeps running, as a loser of `velt_rt_race` does (§1.1). Either way its result is disposed of with `quiet_drop` (handled, null: nothing to drop). Any other future is dropped as by `velt_rt_fut_drop`. Takes ownership of `f`. |
| `velt_rt_fut_share` | `(VeltFut* f, u64 result_size, void (*share)(const void* src, void* dst), void (*result_drop)(void* slot)) -> VeltFut*` | another holder of promise `f` (§1.2): a new handle on `f`, which is now shared; null stays null. `share` writes another reference to a result into a slot (null: the bytes are copied), `result_drop` drops one (null: nothing to drop) (additive) |
| `velt_rt_fut_peek` | `(VeltFut* f) -> u8` | what `console.log` shows of a promise its owner holds: `1` when its result is in the slot (+16; a `Result` tag first for a promise that can reject), `0` while pending. Polls, claims and moves nothing. A future that only runs when awaited (a runtime leaf, a join handle, a combinator, a promise created outside a task) reads as pending (additive) |
| `velt_rt_futs_handled` | `(VeltFut* const* futs, u64 n, void (*quiet_drop)(void* slot))` | the `n` futures are handed to a combinator (`race`, `any`, `all`), which handles their rejections like JS: a started promise among them that is dropped unfinished later disposes of its result with `quiet_drop` (null: nothing to drop) instead of its `result_drop`, so a rejection is not reported (§1.1). No-op for other futures. Called before the combinator takes the futures. |

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
- `velt_rt_fut_poll` on a started promise: READY once it finished (the awaiter then owns the
  result at +16). Polled from inside the task that drives it while it is not woken, the awaiter
  polls its state directly with its own `cx` ("adopts" it: its leaves then wake the awaiter, with
  no trip through the set — e.g. `Promise.all` over stored promises); otherwise the awaiter's
  waker is registered and the set drives it. Any task may await it. An adopted promise that is
  dropped, or awaited from elsewhere, goes back to its set.
- `velt_rt_fut_drop` on an unfinished started promise does **not** cancel it: it keeps running
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

A promise is a shared value (docs/reference/async.md "Promises"): where sema shares a promise
place (`Intrinsic::Share`, or a clone of a value holding a promise), lowering calls
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
  `velt_rt_futs_handled`) treat a holder as some other future. `velt_rt_fut_peek` reads the
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
- A shared promise stays on the logical task where it was shared (`velt_rt_task_id`: the
  orphan task driving a task's promises after its root returned has the same one; on
  WebAssembly, the task a started promise runs for). A holder that is polled, shared, peeked or
  dropped on another task without having been handed over (above) stops the program with a
  fatal error (exit code 101), unless the thread holds the module lock (§ module state; on
  WebAssembly: the program has module state). That is all the check tests. It is sound because
  the task the promise is shared on holds the lock whenever it runs too: a holder reaches
  module state only through code holding the lock, so the task it is shared on put it there
  or first used it from there, and is a *touched* task from then on, whose every poll takes
  the lock; the outside home (below) is a touched task in every program with module state. So
  the tasks using a holder read from module state (a `Map` of promises in flight), its
  result, and the objects reached from that run one at a time.
- A holder handed over outside a task (a synchronous `main`) gets its proxy in the *outside
  home*, a set with a task id of its own that code outside a task enters for each hand-over,
  so every holder handed over from there is one of the same task. When `main` returns, the
  home's set moves to an orphan task, which drives it (and keeps the process alive) until its
  promises finish. In a program with module state the home's polls take the module lock, here
  and in the orphan, as any touched task's do.
- A started original's result is claimed (`OWNER_DELIVERED`) only under the hub's lock, by the
  hub's waker on the promise's task or by the holder that saw it finish, whichever comes first;
  the other finds it settled.

## 2. Tasks

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_block_on` | `(PollFn poll, void* state)` | `async main`, and a synchronous `main` of a program that starts promises or tasks (§1.1; its root calls `main`): `velt_main` builds the state on its stack, calls this, then reads the result at `state+0`. Runs the root as a task on the runtime's workers; returns when READY. Tasks still running afterwards are abandoned when the process exits. Must not be called from inside a task. |
| `velt_rt_tla_at` | `(const VeltStr* site)` | a top-level `await` (std/prelude/process.vlt `__topLevelAwaitAt`) at `site` (`path:line:col`, a string literal) suspends: called on the suspending path only (an `await` that goes on at once calls nothing), two atomic stores of the literal's bytes, no lock, no allocation. When the root can never finish, the site stored last makes the program write `Warning: Detected unsettled top-level await at <path>:<line>` and exit with 13, as Node does; with none stored it exits with 0 (`top_level_await.rs`, both runtimes) |
| `velt_rt_spawn` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | `spawn(f(...))`: copies the initial state into the task (caller gives up ownership of its contents), starts it now. Returns the join handle; its result slot (+16) receives `result_size` bytes (≤ 256; box larger results). Dropping the handle **detaches** (task keeps running); a result the handle never claims (dropped before or after the task finished) is dropped with `result_drop` (null: nothing to drop). |
| `velt_rt_spawn_transfer` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot), void (*result_transfer)(void* slot)) -> VeltFut*` | `velt_rt_spawn` for a result that can reach counted objects: `result_transfer` (compiled transfer glue, in place) runs on the result as the task's state finishes, on the task and inside its local set (so promises the task started, which may still use the result's objects, are on the same thread, and a promise the glue starts joins the set), before the join handle can see it. Null: as `velt_rt_spawn`. |
| `velt_rt_spawn_detached` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align)` | spawn whose result is unused: no handle, one allocation |
| `velt_rt_keep_alive_acquire` | `()` | takes a keep-alive reference (§7): a ref'd timer is pending (std/prelude/timers.vlt). Additive. |
| `velt_rt_keep_alive_release` | `()` | releases one reference taken with `velt_rt_keep_alive_acquire`; the caller keeps them balanced. On WebAssembly, `velt_rt_block_on` returns only once none is held. |
| `velt_rt_timer_unref_due` | `() -> VeltFut*` (result `bool`) | a due `unref()`'d timer asks whether to drop its callback: true once the process was seen with nothing left that can happen (quiet.rs), as Node exits without running it (std/prelude/timers.vlt). While nothing but running code may be left (a thread waits for the end, no keep-alive reference, no timer or I/O pending, nothing queued), it waits until the last worker parks, which records the end or wakes it. Ready at once with false on WebAssembly, whose one thread runs the timers and decides the end. |
| `velt_rt_spawn_fut` | `(VeltFut* f, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | `spawn(p)` where `p` is already a heap future (boxed promise, leaf); takes ownership of `f`; `result_drop` as for `velt_rt_spawn` |
| `velt_rt_spawn_target` | `(i64 worker)` | `spawn(p, { worker })` (#211): the next spawn on this thread pins its task to `worker`. Called right after the task's arguments are evaluated, immediately before `velt_rt_spawn` / `_transfer` / `_detached` / `_fut`. Out of range (`worker < 0` or `≥ velt_rt_worker_count()`): panic `spawn: worker <w> is out of range (the runtime has <n> workers)`. With the stealing scheduler the task is spawned as usual. Additive. |
| `velt_rt_worker_count` | `() -> u64` | `workerCount()`: the per-core workers, or the stealing runtime's worker threads (1 on WebAssembly). Additive. |
| `velt_rt_current_worker` | `() -> u64` | `currentWorker()`: the per-core worker running this task; with the stealing scheduler an index for this worker thread. Additive. |
| `velt_rt_set_scheduler` | `(u32 kind)` | `velt_main` calls it with `1` first, before anything starts the runtime, when the program pins a task (it references `velt_rt_spawn_target`). A no-op: the default scheduler keeps pinned tasks on their workers itself (kept for programs compiled to call it). Additive. |

Runtime: created lazily, workers = `VELT_THREADS` (positive integer) or the number of cores.
The per-core scheduler (#211; `VELT_SCHEDULER=per-core`) makes every worker a one-worker multi-thread runtime with
its own I/O and timer drivers; a task stays on the worker it started on, and an unpinned spawn
starts on the spawning worker unless that worker runs 2 more tasks than the least loaded one
(`task/runtime/`). A connection's socket and its tasks stay on one worker: `accept()`
registers the connection with the caller's worker, and `serve`'s accept loop spawns each
connection where placement puts it (`runtime::spawn_with`), moving its socket to that worker's
driver only when it is another worker. A listening socket has one registration (per-worker
duplicates woke every worker for each connection, and on Windows could block a worker in
`accept`). `stealing` (`VELT_SCHEDULER=stealing`) is one multi-thread runtime. Per-core trade-offs today: a task never moves after its first poll, so a
CPU-bound task delays every task on its worker; placement counts live tasks, not CPU; a socket
stays on the worker whose driver registered it; spawn bursts and cross-worker request/reply are
much slower than stealing (no batch steal-before-start yet, #997).

Every backend sits behind `task/runtime/` (`spawn`, `spawn_with`, `spawn_blocking`, `enter`,
`block_on`, and its own `JoinHandle`/`AbortHandle`); nothing else in the runtime names a backend's
types. The default (`VELT_SCHEDULER` unset or `velt`; `stealing` and `per-core` select tokio's;
`velt_rt_set_scheduler` selects nothing any more) is the runtime's own executor
(`task/runtime/velt/`): worker threads with their
own queue and `mio` poll; a task woken by I/O, a timer or another thread queues on its home
worker, one woken by a running task runs next on the waker's worker (a one-task slot, at most 3
in a row; it keeps its home), and a spawn stays on the spawning worker (from outside the
workers: worker 0). Each worker's run queue (`velt/queue.rs`) runs input (data to read, timers,
other threads) before output and the work tasks made (room to write, spawns, budget yields),
each in order; every 61st task comes from the second
queue, and once it has waited 10 ms a fair pass runs all of it. A worker whose input has a
delay of 200 µs or more over a 10 ms window (the mean input queue noted in the window times the
mean wall time of a poll; the whole queue once polls average 20 µs), or that was awake for 95 %
of the window with input waiting, offers half of its
queue to a sleeping worker, which becomes the tasks' home (`velt/spread.rs`). A worker that runs
out of work, before it sleeps, runs half of the input waiting on another awake worker without
taking it over (the tasks keep their home). The monitor does the duties of a worker that has
started no poll for 2 ms while awake: a turn of its reactor and timers, and its inbox and half
of its queue to another worker, each 1 ms tick while the stall lasts. A connection's socket
moves its registration to its task's home worker when the task reads there (Unix). A Postgres
pool hands a task an idle connection whose protocol task last ran on the task's worker first. A worker
with 256 or more queued tasks polls its reactor every 4 tasks.
TCP sockets (`net/sys.rs`), the HTTP server, Postgres over TCP and timers are native; tokio
types elsewhere (files, processes, Postgres over a Unix socket, Redis, the HTTP client, UDP, DNS,
`spawn_blocking`) run on a one-thread tokio runtime whose context every worker stays inside.
Allocation per task: on the Velt executor, 3 (the boxed future with its state, the task and the
join state shared with the handle). On tokio's schedulers, states ≤ 1 KiB (align ≤ 16) live
inline in tokio's task cell (size classes 64/256/448/576/1024), so a detached spawn is 1
allocation and a joinable one 2 (task + join handle).
Panics inside tasks print `panic: <msg>` and exit 101 (same hook as sync code).

### 2.1 Latches (`new Promise`)

A one-shot latch (`u64` handle, §3.2): the waking half of `new Promise` (std/prelude/promise.vlt
keeps the settled value in Velt code). Thread-safe; no callbacks are stored (§13.5).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_latch_new` | `() -> u64` | a closed latch |
| `velt_rt_latch_free` | `(u64 latch)` | releases the handle (a pending wait keeps the latch alive) |
| `velt_rt_latch_open` | `(u64 latch)` | opens it and resumes every waiter; a no-op when open. A waiter of the task being polled (its root or one of its started promises) is queued in that task's microtask queue now, as Node queues a promise's reactions inside `resolve`; that entry is the waiter's `await` step. Other waiters are woken. |
| `velt_rt_latch_wait` | `(u64 latch) -> VeltFut*` | completes (unit result) once the latch is open, and for a waiter queued by `velt_rt_latch_open`, once its queue reached that entry |
| `velt_rt_task_id` | `() -> u64` | a unique id of the task being polled, never reused and kept by its local promises when they outlive it (0 outside a task; always 1 on single-threaded wasm): `resolve` copies a value settled from another task |

### 2.2 Channels (`velt:channel`)

`Channel<T>` (std/channel.vlt) is a Copy struct around a `u64` handle, a key into a runtime table
like the socket handles (§3.2): a channel leaves the table once it is closed and drained, and any
later use of a copy sees a closed, empty channel. The runtime never sees a `T`, only its bytes:
every call passes the item size (align <= 16). `receive` results are a `T | null` in the
compiler's layout: `payload` is the offset of the value after the `bool` present flag, or 0 when
`T` is pointer-like and null is the zero pointer. `send`, `trySend`, `receive` and `tryReceive` are reached
through the std-only intrinsics `__intrinsic_chan_{send,try_send,receive,try_receive}<T>` (lowering knows
`T`'s size, layout and drop glue; it transfers the value first, like a `spawn` argument: a value the
sender still shares is deep-copied). No code pointers are stored except drop glue (§13.5): the
`item_drop` a pending `send` future owns, and the channel's copy of it. A channel keeps the
`item_drop` of the first `send`/`trySend` that passed one (every item of a `Channel<T>` shares it).
Values still queued in a channel nobody drains, closed or not, are not dropped while the program
runs (any copy of the handle may still receive them); when it ends (`main` returned 0 and no
keep-alive reference remains, §7; with `VELT_RC_STATS=1` a debug runtime also waits up to a
minute for running tasks) the runtime drops every item still queued in any channel with that function (not after a failing exit; the `VELT_RC_STATS` report
counts them as `channel leftovers=<n>`). velt_rt_wasm does not: the instance's memory goes with
it.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_chan_new` | `(u64 capacity) -> u64` | a new channel; `capacity == 0` is unbounded |
| `velt_rt_chan_close` | `(u64 ch)` | closes it: sends fail (pending ones too), receivers drain then get null; idempotent |
| `velt_rt_chan_closed` | `(u64 ch) -> bool` | |
| `velt_rt_chan_len` | `(u64 ch) -> u64` | queued items |
| `velt_rt_chan_send` | `(u64 ch, const void* src, u64 size, void (*item_drop)(void*)) -> VeltFut*` | moves the item's bytes (and ownership) into the future at the call; `bool` result: queued (after waiting for space), or false when closed. An item not queued is dropped with `item_drop` (null: nothing to drop) |
| `velt_rt_chan_try_send` | `(u64 ch, const void* src, u64 size, void (*item_drop)(void*)) -> bool` | moves the item's bytes into the channel if it has room now; false when full or closed, and the item is dropped with `item_drop` |
| `velt_rt_chan_receive` | `(u64 ch, u64 size, u64 payload, u64 slot_size) -> VeltFut*` | result: a `slot_size`-byte `T \| null` (see above), null once closed and drained |
| `velt_rt_chan_try_receive` | `(u64 ch, void* dst, u64 size, u64 payload)` | writes the oldest item, or null, as a `T \| null` at `dst` |

**Process-wide slots.** A module has no mutable globals; a std feature that needs one object
per process (std/websocket's topics, slot 0) keeps a `shared<T>` in a runtime slot. Only the
std module that owns a slot touches it, with the slot's one type: there is no generic accessor
an importer could use to read a slot as another type. A slot is an unbounded channel holding at most one item, since a channel is what can own
a typed Velt value: std locks the slot, takes the item out (making it the first time), puts a
reference back and unlocks. The item is dropped with the channel's leftovers when the program
ends. No suspension point may lie between lock and unlock.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_global_lock` | `(u32 slot)` | locks slot `slot` (0..8; std assigns them) |
| `velt_rt_global_unlock` | `(u32 slot)` | unlocks it, on the thread that locked it |
| `velt_rt_global_chan` | `(u32 slot) -> u64` | the slot's channel, made on first use; called with the slot locked |

### 2.3 Abort signals (`velt:task`)

Each signal's `u64` handle (an `Arc`) is owned by a private `shared` cell in std/task.vlt and
released once, at the cell's last reference; no handle is public. Aborting sets a flag, stores
the reason and wakes the waiters; it never cancels anything itself. No code pointers are
stored: `AbortSignal.timeout` is a runtime timer task holding a weak reference, aborted (freed with
its timer) when the signal is dropped. A signal made
by `any` holds strong references to its sources until it is aborted (they hold weak ones back).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_signal_new` | `() -> u64` | a signal that is not aborted |
| `velt_rt_signal_retain` | `(u64 s) -> u64` | another reference to it (a scope's child task holds one) |
| `velt_rt_signal_free` | `(u64 s)` | releases a handle |
| `velt_rt_signal_abort` | `(u64 s, const VeltStr* reason)` | aborts it and the signals derived from it (`any`); a no-op if aborted |
| `velt_rt_signal_aborted` | `(u64 s) -> bool` | |
| `velt_rt_signal_reason` | `(u64 s, VeltStr* out)` | the reason (`""` while not aborted) |
| `velt_rt_signal_timeout_ms` | `(u64 s) -> i64` | the `ms` of the `velt_rt_signal_timeout` signal that aborted it (directly or through `any`), else -1 |
| `velt_rt_signal_wait` | `(u64 s) -> VeltFut*` | completes (unit) once aborted |
| `velt_rt_signal_timeout` | `(i64 ms, const VeltStr* reason) -> u64` | a signal a timer aborts after `ms` |
| `velt_rt_signal_any` | `(const VeltArray<u64>* signals) -> u64` | aborted with the first of `signals` that is (its reason and timeout); keeps them alive until then |

### 2.4 Task groups (`taskScope`)

The live-children count of a `taskScope` (std/task.vlt), behind a registry key (a `TaskScope`
copy used after its scope ended finds no group). No code pointers are stored.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_group_new` | `() -> u64` | a group with no children |
| `velt_rt_group_enter` | `(u64 g) -> bool` | a child is about to start; false (nothing counted) once the group is closed or freed |
| `velt_rt_group_leave` | `(u64 g)` | a child finished |
| `velt_rt_group_wait` | `(u64 g) -> VeltFut*` | completes (unit) once no child is live, and closes the group then |
| `velt_rt_group_free` | `(u64 g)` | the scope ended |

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
- `IoError.code` (a string in Velt): `velt_rt_err_code_name(i32 code, VeltStr* out)` writes a static
  string (nothing to free).

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

`std/*.vlt` binds runtime functions with `declare function`, which the compiler lowers like a
Velt function: scalars (`bool` = `u8`, `number` = `f64`, the sized integers) stay scalars; every
other value (string, array, struct, tuple, closure) is passed as a pointer to it; `Promise<T>`
is a `VeltFut*`; a non-scalar result becomes a **trailing out-pointer and a `void` return**. The
runtime's definitions must be exactly that signature: native linkers tolerate an ignored return
value or a `u64` where a 64-bit pointer is expected, but WebAssembly links only identical
signatures (wasm32 pointers are 32 bits). `crates/velt_rt/tests/std_externs.rs` compares every
declaration in `std/` with velt_rt's and velt_rt_wasm's definitions.

### 3.2 Object handles

Runtime objects that Velt code holds (`VeltListener`, `VeltStream`, `VeltServer`, `VeltReq`,
`VeltResp`, `VeltFetchResp`, `VeltRegex`, `VeltChild`, `VeltUdp`, `VeltFileReader`,
`VeltFileWriter`, `VeltJson`) are **`uint64_t` handles**, 0 = none. Velt has no
pointer type, so `std` stores them in `u64` fields and passes `u64` arguments; the runtime takes
and returns the same `u64`, in argument lists, results and result slots alike.
- Handles that `std` wraps in **Copy structs** (`VeltListener`, `VeltStream`, `VeltUdp`,
  `VeltChild`, `VeltFileReader`, `VeltFileWriter`) are keys into runtime handle tables
  (`velt_rt::registry::Key<T>`: generation, shard and slot), because Velt code may hold several copies:
  releasing one (`close`/`free`) makes every copy dead. Later operations fail with `EBADF`
  (code 16, "handle is closed"), sync accessors return their documented empty value (port 0,
  pid 0, exit code -1, empty address), and releasing again is a no-op; a stale handle never
  reaches a newer object. In-flight operations hold their own `Arc`, so the object lives until
  they finish.
- The HTTP handles (`VeltServer`, `VeltReq`, `VeltResp`, `VeltFetchResp`) are registry keys as
  well, so a stale or forged one never reaches memory (additive): server operations on a closed
  handle are no-ops (port 0, `shutdown` resolves at once), a dead response key is ignored
  (`velt_rt_http_resp_drop`) and the server answers 500 for one, and using a
  released request or fetch response is a fatal error that says so.
- The others are the object's address (`velt_rt::handle::Handle<T>`, `repr(transparent)`): an
  `Arc` (regexes, JSON nodes) owned by a class whose `dispose()` releases it exactly once. std
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
| `velt_rt_bytes_drop` | `(VeltBytes* b)` — frees if `cap > 0`, zeroes |
| `velt_rt_bytes_from_str` | `(const VeltStr* s, VeltBytes* out)` — copy |
| `velt_rt_bytes_to_str` | `(const VeltBytes* b, IoResult<VeltStr>* out)` — copy, `EILSEQ` if invalid |
| `velt_rt_bytes_to_str_lossy` | `(const VeltBytes* b, VeltStr* out)` — copy, an invalid sequence as U+FFFD (WHATWG, as `utf8DecodeLossy`); additive |
| `velt_rt_bytes_is_utf8` | `(const VeltBytes* b) -> u8` — whether `b` is valid UTF-8; additive |
| `velt_rt_str_array_drop` | `(VeltStrArray* a)` — drops elements + buffer, zeroes |

## 5. std/fs

Arguments are copied at the call (the caller keeps ownership). Paths are UTF-8 strings. Data
arguments are strings (`VeltStr`).

```c
typedef struct { uint64_t size; double mtime_ms; uint8_t is_file; uint8_t is_dir;
                 uint8_t is_symlink; } VeltStat;                 // 24 bytes; is_symlink: lstat only
typedef struct { VeltStrArray names; VeltBytes kinds; } VeltDirents;  // 48 bytes [additive, #692]
```

`VeltDirents` holds the sorted names and, at the same index, each entry's kind: 0 other (FIFO,
socket, device), 1 file, 2 directory, 3 symlink (not followed). The kind comes with the listing
(`d_type`, or Windows' find data); only an entry of unknown type costs an `lstat`.

| Async (`-> VeltFut*`) | Sync (`..., out)`, returns nothing) | Result type |
|---|---|---|
| `velt_rt_fs_read_file(path)` | `velt_rt_fs_read_file_sync(path, out)` | `IoResult<VeltStr>` (`EILSEQ` if not UTF-8) |
| `velt_rt_fs_read_file_bytes(path)` | `velt_rt_fs_read_file_bytes_sync(path, out)` | `IoResult<VeltBytes>` |
| `velt_rt_fs_write_file(path, data)` | `velt_rt_fs_write_file_sync(path, data, out)` | `IoResult<()>` (= `VeltErr`) |
| `velt_rt_fs_append_file(path, data)` | `velt_rt_fs_append_file_sync(path, data, out)` | `IoResult<()>`; creates the file |
| `velt_rt_fs_read_dir(path)` | `velt_rt_fs_read_dir_sync(path, out)` | `IoResult<VeltStrArray>` names, sorted |
| `velt_rt_fs_read_dir_typed(path)` | `velt_rt_fs_read_dir_typed_sync(path, out)` | `IoResult<VeltDirents>` [additive, #692] |
| `velt_rt_fs_stat(path)` | `velt_rt_fs_stat_sync(path, out)` | `IoResult<VeltStat>` (follows symlinks) |
| `velt_rt_fs_lstat(path)` | `velt_rt_fs_lstat_sync(path, out)` | `IoResult<VeltStat>` (a symlink itself) [additive, #691] |
| `velt_rt_fs_readlink(path)` | `velt_rt_fs_readlink_sync(path, out)` | `IoResult<VeltStr>`; `EINVAL` if not a symlink [additive, #691] |
| `velt_rt_fs_symlink(target, path)` | `velt_rt_fs_symlink_sync(target, path, out)` | `IoResult<()>`; Windows: a directory link if `target` is a directory, a relative target stored with `\` separators (as Node) [additive, #691] |
| `velt_rt_fs_mkdir(path, u8 recursive)` | `velt_rt_fs_mkdir_sync(path, recursive, out)` | `IoResult<()>` |
| `velt_rt_fs_remove(path, u8 recursive)` | `velt_rt_fs_remove_sync(path, recursive, out)` | `IoResult<()>`; file, symlink (the link only, a Windows directory link too), empty dir, or tree if recursive |
| `velt_rt_fs_rename(from, to)` | `velt_rt_fs_rename_sync(from, to, out)` | `IoResult<()>` |
| `velt_rt_fs_copy_file(from, to)` | `velt_rt_fs_copy_file_sync(from, to, out)` | `IoResult<()>` |
| `velt_rt_fs_exists(path)` | `velt_rt_fs_exists_sync(path) -> u8` | `u8` (never fails) |

All `path`/`from`/`to`/`data` parameters are `const VeltStr*`. Async variants run on tokio's
blocking pool. Error messages are Node's: `<CODE>: <description>, <syscall> '<path>'` (plus
` -> '<to>'` for `rename`/`copyfile`/`symlink`), e.g. `ENOENT: no such file or directory, lstat 'x'` from
`fs_remove`; the file streams' `open_read`/`open_write` (§14.7) use the same form. A failed
read or write of an opened file names no path (`EISDIR: illegal operation on a directory,
read`), and a directory opened as a file is `EISDIR` on every system (Windows reports access
denied).

## 6. std/net (TCP, TLS over TCP)

Handles (`VeltListener`, `VeltStream`: §3.2) are `Arc`s; in-flight operations keep the socket
alive, so `close` may be called at any time. A `VeltStream` is a raw TCP socket, a TLS stream
(rustls, §14.8) or an HTTP connection handed over by an upgrade (`velt_rt_http_req_take_upgrade`,
§14.8); every operation below works on all of them, and one task may read while another writes.

| Symbol | Signature | Result slot / notes |
|---|---|---|
| `velt_rt_tcp_listen` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<VeltListener>`; `addr` = `"host:port"`, port 0 = any free port |
| `velt_rt_tcp_listener_port` | `(VeltListener l) -> u32` | the real bound port |
| `velt_rt_tcp_listener_close` | `(VeltListener l)` | |
| `velt_rt_tcp_accept` | `(VeltListener l) -> VeltFut*` | `IoResult<VeltStream>` (TCP_NODELAY on) |
| `velt_rt_tcp_connect` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<VeltStream>` |
| `velt_rt_tcp_read` | `(VeltStream s, u64 max) -> VeltFut*` | `IoResult<VeltBytes>` 1..max bytes, empty = EOF; `max == 0` ⇒ 64 KiB |
| `velt_rt_tcp_read_string` | `(VeltStream s, u64 max) -> VeltFut*` | `IoResult<VeltStr>` next chunk as UTF-8 (split characters are completed on the next call, invalid bytes → U+FFFD), `""` = EOF |
| `velt_rt_tcp_write` | `(VeltStream s, const VeltStr* data) -> VeltFut*` | `IoResult<()>` after all bytes are written; `data` is copied. Writes go out in call order: a write that cannot finish at the call goes on in its first poll (at the `await`, which a std `write` reaches at its call), and later ones wait for it |
| `velt_rt_tcp_write_bytes` | `(VeltStream s, const VeltBytes* data) -> VeltFut*` | same for a `u8[]` |
| `velt_rt_tcp_try_read_into` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end, IoResult<i64>* out)` | `readInto`: reads what is there into `data[start..end]` (a `u8[]` the caller lends for the call and the runtime writes; sema's written-parameter list has it), without waiting: the bytes read, 0 at the end of the stream, -1 when nothing has arrived yet. A raw socket reads straight into the range; a TLS or upgraded stream hands over what `velt_rt_tcp_readable` read ahead. An empty or out-of-range range reads nothing (0). Additive |
| `velt_rt_tcp_readable` | `(VeltStream s) -> VeltFut*` | result `IoStatus` once a `velt_rt_tcp_try_read_into` may find data, the end of the stream or an error (it may also find nothing yet: try again); holds no buffer of the caller's. A TLS or upgraded stream reads its next chunk (up to 64 KiB) ahead here. `read` / `readString` deliver what was read ahead first. Additive |
| `velt_rt_tcp_readable_within` | `(StreamHandle s, int64_t ms, uint64_t signal) -> VeltFut*` (`IoResult<u8>`) | 1 once a `tryRead` may find something, 0 when `ms` passed first (`ms < 0`: no limit) or the signal (0: none) was aborted; additive |
| `velt_rt_tcp_write_range` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end) -> VeltFut*` | `velt_rt_tcp_write_bytes` of `data[start..end]` (clamped): a `Uint8Array`'s range. Additive |
| `velt_rt_tcp_try_write_range` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end) -> u64` | `velt_rt_tcp_try_write` of `data[start..end]` (clamped). Additive |
| `velt_rt_tcp_write_later` | `(VeltStream s, const VeltStr* data, IoResult<u64>* out)` | queues `data` (copied), sent after the current task poll with everything else queued on the stream; the bytes still to be sent; fails once sending failed, after `abort`, on a closed handle. Additive |
| `velt_rt_tcp_write_later_range` | `(VeltStream s, const VeltBytes* data, u64 start, u64 end, IoResult<u64>* out)` | `velt_rt_tcp_write_later` of `data[start..end]` (clamped). Additive |
| `velt_rt_tcp_drained` | `(VeltStream s, u64 max) -> VeltFut*` (`IoResult<()>`) | once at most `max` queued bytes (and writes after them) are still to be sent, or the error that stopped sending. Additive |
| `velt_rt_tcp_try_write` | `(VeltStream s, const VeltBytes* data) -> u64` | writes what a raw socket takes right now, without waiting; the number of bytes written (0: nothing now, an earlier write still going on, a closed handle, a TLS stream or one not set up yet, or an error, which the next write reports) |
| `velt_rt_tcp_shutdown` | `(VeltStream s, VeltErr* out)` | half-close (peer reads EOF) |
| `velt_rt_tcp_peer_addr` | `(VeltStream s, VeltStr* out)` | `"ip:port"` |
| `velt_rt_tcp_abort` | `(VeltStream s)` | operations in flight and every later one fail with `ECONNABORTED` (a raw socket is shut down both ways, which ends a waiting read; the read then reports the abort, not EOF); the handle stays open |
| `velt_rt_tcp_close` | `(VeltStream s)` | releases the handle |
| `velt_rt_tcp_listen_tls` | `(const VeltStr* addr, const VeltStr* cert_pem, const VeltStr* key_pem, const VeltStr* alpn) -> VeltFut*` | `IoResult<VeltListener>` whose accepted streams are TLS; `alpn` = protocols to offer, comma-separated (`""`: none); bad PEM ⇒ `EINVAL`. `velt_rt_tcp_accept` returns once the TCP connection is accepted: the handshake (10 s limit) runs with the stream's first operation, which reports its failure |
| `velt_rt_tcp_connect_tls` | `(const VeltStr* addr, const VeltStr* server_name, const VeltStr* ca_pem, const VeltStr* alpn) -> VeltFut*` | `IoResult<VeltStream>` once the handshake is done; `server_name` `""` = the host of `addr`; `ca_pem` = extra trusted CAs |

## 7. std/http (hyper 1.x, HTTP/1.1 keep-alive + h2c)

**Handler.** A handler `(req: Request) => Promise<Response>` is described by:
```c
typedef struct {
    void     (*init)(void* env, void* req, void* state);    // write the initial state; owns req
                                                            // (the VeltReq handle as a pointer)
    uint32_t (*poll)(void* state, void* cx);                // result at state+0: VeltResp, or 1 =
                                                            // responded (req_respond); 0 ⇒ 500
    void     (*drop)(void* state);                          // request cancelled (client gone)
    uint64_t state_size, state_align;
    void*    env;   // closure captures; shared read-only by concurrent requests (§14.14)
} VeltHandler;      // 48 bytes
```
The runtime stores each request's state inline in the request future (no extra allocation for
states ≤ 1 KiB) and polls it on a worker. `init` runs once the request's head has arrived; the
body is received while the handler reads it (`req_text`, `req_bytes`, `req_chunk`).
The handler must `velt_rt_http_req_drop(req)` when done with it (typically before returning).
`VeltReq` is a registry key (`crate::registry`, like the database handles): every accessor checks
it, and using a request after it was dropped stops the program with a clear message instead of
reading freed memory. While the runtime polls a handler it keeps that handler's request in a
per-worker frame: accessors given its key read it there (no registry lock or count), and
`velt_rt_http_req_drop` takes it out of the frame as well as out of the registry, so the check
still holds. Any other key (a request handed to another task) is looked up in the registry.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_serve` | `(const VeltStr* addr, const VeltHandler* h) -> VeltFut*` | result `IoResult<VeltServer>` once bound; serving continues in the background. `h` is copied. |
| `velt_rt_http_server_port` | `(VeltServer s) -> u32` | real port (after port 0) |
| `velt_rt_http_server_close` | `(VeltServer s)` | stop accepting, finish in-flight requests, close idle connections; frees `s` |
| `velt_rt_http_server_detach` | `(VeltServer s)` | free `s` but keep serving (dropping the `Server` value) |
| `velt_rt_http_server_shutdown` | `(VeltServer s) -> VeltFut*` | `close` + result `()` once the handler was released (§14.14) |
| `velt_rt_http_req_method` | `(VeltReq r, VeltStr* out)` | owned copy |
| `velt_rt_http_req_url` | `(VeltReq r, VeltStr* out)` | the absolute URL: `http://` (`https://` over TLS), the `host` header (HTTP/2: `:authority`; neither: the server's address), path and query |
| `velt_rt_http_req_header` | `(VeltReq r, const VeltStr* name, VeltStr* out) -> u8` | case-insensitive; repeated fields joined with `", "`; 0 = absent (`out` untouched) |
| `velt_rt_http_req_headers` | `(VeltReq r, VeltStrArray* out)` | every header as the flat list `[name, value, …]` (lowercase names, received order; non-UTF-8 values decoded lossily): `req.headers` in one call |
| `velt_rt_http_req_has_body` | `(VeltReq r) -> u8` | whether the request arrived with a body |
| `velt_rt_http_req_text` | `(VeltReq r) -> VeltFut*` | result `IoResult<VeltStr>`: the rest of the body, UTF-8 decoded lossily, a leading BOM dropped |
| `velt_rt_http_req_bytes` | `(VeltReq r) -> VeltFut*` | result `IoResult<VeltBytes>`: the rest of the body |
| `velt_rt_http_req_chunk` | `(VeltReq r) -> VeltFut*` | result `IoResult<VeltBytes>`: the next chunk (≤ 64 KiB, never empty); empty at the end |
| `velt_rt_http_req_remote_host` | `(VeltReq r, VeltStr* out)` | the client's IP address |
| `velt_rt_http_req_remote_port` | `(VeltReq r) -> u32` | the client's port |
| `velt_rt_http_req_drop` | `(VeltReq r)` | |
| `velt_rt_http_resp_build` | `(u32 status, const VeltStr* reason, const VeltStrArray* headers, u32 kind, VeltStr* text, const VeltBytes* bytes, u32 implied) -> VeltResp` | a handler's `Response` in one call; see below |
| `velt_rt_http_req_respond` | `(VeltReq r, u32 status, const VeltStr* reason, const VeltStr* name, const VeltStr* value, u32 kind, VeltStr* text, u32 implied) -> u64` | `resp_build` with no body or a text one (`kind` 0 or 1) and at most one header (`name` as written, `""` for none, lowercased by parsing; `value` trimmed of HTTP whitespace) for the handler of `r`, handed straight to it: returns 1 (the response is in the handler's frame; the handler returns 1) while the runtime polls that handler, else a `VeltResp` (a handler that runs on after its client left), 0 if a header is invalid |
| `velt_rt_http_req_respond_list` | `(VeltReq r, u32 status, const VeltStr* reason, const VeltStr* name, const VeltStr* value, const VeltStrArray* headers, u32 kind, VeltStr* text, const VeltBytes* bytes, u32 implied) -> u64` | `req_respond` with more headers (`name`/`value` first, then the flat list, as for `resp_build`) and any complete body (`kind` 0–2) |
| `velt_rt_http_resp_drop` | `(VeltResp r)` | only for responses not returned from a handler |

A listening server holds a **keep-alive** reference (released by `close`): after `main` returns,
the program entry waits until no keep-alive references remain — like Node, a listening server
keeps the process running. (`velt_rt_block_on` itself does not wait.) Pending ref'd timers hold
one too (`velt_rt_keep_alive_acquire`, §2).

`velt_rt_http_resp_build` turns the global `Response` a handler returned into a `VeltResp`:
`status` (invalid ⇒ 500), the reason phrase (`""` for the standard one), the headers as the flat
list `[name, value, …]`, and the body by `kind`: 0 none, 1 `text` (**taken**: zero-copy if owned,
`*text` left empty), 2 `bytes` (copied: the array may be borrowed), 3 a stream std fills next
(§14.17), 4 a stream of a fetched response's body (decoded, so a `content-encoding` the client
decoded is dropped). A streamed body drops `content-length`. `implied` is the `content-type`
the body implies, added unless the headers have one: 0 none, 1 `text/plain;charset=UTF-8`, 2
`application/json`, 3 `application/x-www-form-urlencoded;charset=UTF-8`. A bodiless status (1xx,
204, 304) gets no body and no `content-type`. An invalid header name or value builds nothing
(result 0); std answers 500 then.

**Client** (the global `fetch`, std/fetch.vlt; `http://` and `https://`, other schemes fail with
`ENOTSUP`; HTTPS and HTTP/2 as in §14.8). The future is READY once the status and headers have
arrived; the body is received by `resp_text` / `resp_bytes`. Redirects are followed as the Fetch
standard says (at most 20; 303, and 301/302 after a POST, become GET without a body; credentials
are dropped on a cross-origin hop). An abort signal (§2.3) drops the request, or the body being
received, as soon as it is aborted; the operation then fails with `UNKNOWN` (99) and std throws the
signal's `AbortError` / `TimeoutError`. Connecting times out after 10 s (`ETIMEDOUT`). A body
with `content-encoding` `gzip` / `x-gzip`, `deflate` (zlib or raw) or `br`, or a list of up to
five of them (undone last first), is decoded frame by frame for every body read, at most 64 KiB
per chunk; a list naming another coding is passed through; a response to HEAD or CONNECT, or
with status 101, 204, 205 or 304, is never decoded; a body that does not decode (or is cut off)
fails with `EILSEQ` (5). velt_rt_wasm implements the same symbols without a network:
`velt_rt_http_fetch_send` fails with `ENOTSUP` (`fetch failed: WebAssembly programs have no
network access`), and the accessors of a fetched response are unreachable (fatal if reached).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_fetch_send` | `(const VeltStr* method, const VeltStr* url, const VeltStrArray* headers, u32 kind, VeltStr* text, VeltBytes* bytes, u32 redirect, u64 signal, const VeltStr* ca_pem) -> VeltFut*` | result `IoResult<VeltFetchResp>`; `headers` = flat `[name, value, …]` (copied; `accept: */*`, `user-agent: velt` and `accept-encoding: gzip, deflate` (`br, gzip, deflate` for `https://`) added when missing); body = `text` (`kind` 1, moved out: the argument is left empty), `bytes` (`kind` 2, copied), or none (`kind` 0); `redirect` 0 follow, 1 error, 2 manual; `signal` = abort signal handle or 0 (borrowed: the runtime takes its own reference); `ca_pem` = extra trusted PEM CAs (`""` = none; clients pooled per CA text); an invalid URL, method, header name or value fails with `EINVAL` before connecting |
| `velt_rt_http_fetch_resp_status` | `(VeltFetchResp r) -> u32` | |
| `velt_rt_http_fetch_resp_status_text` | `(VeltFetchResp r, VeltStr* out)` | the server's reason phrase, else the standard one, else `""` |
| `velt_rt_http_fetch_resp_url` | `(VeltFetchResp r, VeltStr* out)` | final URL, after redirects, without fragment |
| `velt_rt_http_fetch_resp_redirected` | `(VeltFetchResp r) -> bool` | |
| `velt_rt_http_fetch_resp_headers` | `(VeltFetchResp r, VeltStrArray* out)` | flat `[name, value, …]` (lowercase, received order, lossy UTF-8); the first call takes them, later ones return `[]` |
| `velt_rt_http_fetch_resp_text` | `(VeltFetchResp r) -> VeltFut*` | result `IoResult<VeltStr>`: the whole body, invalid UTF-8 as U+FFFD, a leading UTF-8 byte order mark dropped (as JS decodes); a second body read fails `EINVAL` |
| `velt_rt_http_fetch_resp_bytes` | `(VeltFetchResp r) -> VeltFut*` | result `IoResult<VeltBytes>`: the received buffer (sized from `content-length`, not copied again) |
| `velt_rt_http_fetch_resp_chunk` | `(VeltFetchResp r) -> VeltFut*` | result `IoResult<VeltBytes>`: the next decoded chunk (at most 64 KiB), never empty; `[]` once the body is complete (or taken by another read) |
| `velt_rt_http_fetch_resp_drop` | `(VeltFetchResp r)` | an unread body is dropped (its connection closes); a body read in flight keeps the response alive until it completes |

## 8. Process

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_process_args` | `(VeltStrArray* out)` | UTF-8 (wide APIs on Windows), argv[0] included |
| `velt_rt_process_node_argv` | `(VeltStrArray* out)` | Node's `process.argv`: `[current executable, script, ...args]`; the script is `$VELT_SCRIPT` (read and removed at start-up; `velt run` sets it) or the JIT host's `set_script`, else the executable. wasm: `[program, program, ...args]` (WASI: the module path; the browser passes none, so `["", ""]`) |
| `velt_rt_env_get` | `(const VeltStr* name, VeltStr* out) -> u8` | 0 = unset (`out` untouched) |
| `velt_rt_env_set` | `(const VeltStr* name, const VeltStr* value)` | not synchronized with concurrent env readers |
| `velt_rt_env_remove` | `(const VeltStr* name)` | |
| `velt_rt_env_all` | `(VeltStrArray* out)` | `[name0, value0, name1, value1, …]` in the OS's order; names starting with `=` (Windows' per-drive entries) left out; lossy UTF-8. wasm: WASI's environment, empty in the browser |
| `velt_rt_process_cwd` | `(IoResult<VeltStr>* out)` | |
| `velt_rt_process_chdir` | `(const VeltStr* path, VeltErr* out)` | |
| `velt_rt_isatty` | `(i32 fd) -> u8` | 1 if `fd` is a terminal (Node's `tty.isatty`); cached for 0, 1, 2; on Windows other numbers are 0 [additive, #693] |
| `velt_rt_perf_now` | `() -> f64` | `performance.now()`: ms since process start, monotonic |
| `velt_rt_date_now` | `() -> i64` | `Date.now()`: ms since the Unix epoch |
| `velt_rt_exit` | see rt_abi.md | |
| `velt_rt_memory_rss` | `() -> i64` | `process.memoryUsage().rss`: resident set size in bytes from the OS (`/proc/self/statm`, `task_info`, `GetProcessMemoryInfo`); 0 if unknown. wasm: the linear memory size |
| `velt_rt_memory_heap` | `() -> i64` | `heapUsed`: mimalloc's committed heap bytes (Windows: the process's private committed bytes); without mimalloc, the RSS. wasm: the linear memory size |

## 9. Shared state and helpers (sync)

**[non-additive, #1620]** `velt_rt_atomic_add_i64`, `velt_rt_atomic_load_i64` and
`velt_rt_atomic_store_i64` (`shared<i64>`'s `add`, `get` and `set`) are gone from both runtimes:
lowering emits those operations inline as `velt_atomic_*` externs (rt_abi.md "Atomics"). An
object compiled against them no longer links.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_rc_inc` | `(u64* rc)` | `shared.clone()`; Relaxed (like `Arc`); aborts on overflow |
| `velt_rt_rc_dec` | `(u64* rc) -> u8` | Release; returns 1 (after an Acquire fence) when it reached zero: caller drops + frees |
| `velt_rt_mutex_init` | `(u64* lock)` | the `Mutex<T>` lock word: 8 bytes, align 8, inside the shared object; no destroy needed |
| `velt_rt_mutex_lock` / `velt_rt_mutex_unlock` | `(u64* lock)` | `m.with(f)` = lock; call f; unlock. Blocks the thread; bodies must not await. |
| `velt_rt_mutex_lock_checked` | `(u64* lock)` | `velt_rt_mutex_lock` from code that may run in read mode (§9.1): there it stops the program before locking [additive, #708] |
| `velt_rt_rwlock_init` | `(u64* lock)` | the `RwLock<T>` lock words: 19 words (152 bytes), align 8, at the start of the `RwLock<T>`, then the value (std `prelude/sync.vlt`; §9.2) [additive, #708] |
| `velt_rt_rwlock_read` | `(u64* lock, u64 reader) -> u64 how` | `rw.read(f)`: the lock shared with other readers; the thread is in read mode (§9.1) until the unlock, which takes `how` back. `reader` is `how & ~7` of an earlier `read` or `write` in the same call of the calling function (never across an `await`: the task may continue on another thread), else 0 [additive, #708] |
| `velt_rt_rwlock_read_unlock` | `(u64* lock, u64 how)` | the end of the read that returned `how`, on the same thread (callbacks never await) [additive, #708] |
| `velt_rt_rwlock_write` | `(u64* lock, u64 reader) -> u64` | `rw.write(f)`: the lock exclusively; in read mode it stops the program before locking (§9.1). `reader` as for `read`; the result is this thread's reader state, for the unlock and later calls [additive, #708] |
| `velt_rt_rwlock_write_unlock` | `(u64* lock, u64 reader)` | the end of the `write` that returned `reader` [additive, #708] |
| `velt_rt_lk_mode` | `() -> u8` | 1 when this thread is in read mode (a thread-local read, §9.1) [additive, #708] |
| `velt_rt_lk_leave` | `(u64* count, void* obj, void (*drop)(void** slot))` | the reader's own lock-set object `obj`, whose count word is at `count`, is transferred out of a `read` callback: clears `LOCAL` unless the frame it goes to still runs in read mode, that frame being `velt_rt_lk_leave_to`'s number of callbacks out (1 by default: a nested `read`'s result stays the outer reader's). One a nested `read` stores into code outside every read keeps `LOCAL` until the thread's outermost read ends (the callbacks around it may still change it), with one more reference meanwhile, which `drop` (its drop glue) releases once the bit is cleared; transferred again before then, at any depth, it stays so (its count never drops to 1, so a transfer to another thread copies it) (§9.1) [additive, #1523] |
| `velt_rt_lk_keep_made` | `(u64* count)` | a transfer started by `velt_rt_xfer_keep_begin` moves the reader's own object whose count word is at `count`, which the reader may still reference: its count becomes 1 (the caller's reference) without `LOCAL`, unless `velt_rt_lk_leave` would keep the bit until the reads end, or already did for this object, when the count stays (§9.1) [additive, #1523] |
| `velt_rt_lk_share_made` | `(u64* count)` | a transfer started by `velt_rt_xfer_share_begin` keeps the reader's own object whose count word is at `count`, which the reader still references: going to code outside every read (at any depth, `velt_rt_lk_leave_to`), its count becomes 1 (the caller's reference) without `LOCAL`, so the readers' other references borrow it uncounted and a change through them stops the program (`velt_rt_lk_write`); one a nested read's store already keeps the readers' own (`velt_rt_lk_leave`) stays so, and one going to a frame still in read mode stays that reader's own, counted (§9.1) [additive, #1541] |
| `velt_rt_lk_leave_to` | `(u64 levels) -> u64` | the transfers that follow move into a frame `levels` `read` callbacks out from the running one (`u64::MAX`: code outside every read; `0`: the running callback itself, so the object stays the reader's own and keeps `LOCAL`, for a copy lent to a method of the lock's value or of a temporary the reader made), for a store into a container a nested callback captured; returns the number it replaces, which the caller puts back after them (§9.1) [additive, #1523] |
| `velt_rt_lk_write` | `(u64* count)` | a write of the lock-set object whose count word is at `count`: stops the program in read mode unless the reader made the object (§9.1) [additive, #708] |
| `velt_rt_free_after_reads` | `(void* obj, void (*drop)(void** slot))` | the last reference to the lock-set object `obj` (count still 1) went in code changing the caller's containers inside a read: `drop` (its drop glue) frees it now, or in read mode once this thread's outermost read ends (§9.1) [additive, #708] |
| `velt_rt_str_hash` | `(const VeltStr* s) -> u64` | string Map/Set keys; fixed seed (deterministic), not flood-resistant |
| `velt_rt_bytes_hash` | `(const VeltBytes* b) -> u64` | `velt_rt_str_hash` of the string whose bytes are `b` (`Map.getUtf8`: a key looked up by its UTF-8 bytes); additive |
| `velt_rt_str_eq_bytes` | `(const VeltStr* s, const VeltBytes* b) -> u8` | whether `s`'s bytes are `b`; additive |
| `velt_rt_math_sqrt` / `_floor` / `_ceil` / `_round` / `_trunc` / `_fabs` | `(f64) -> f64` | `round` = JS `Math.round` (ties toward +∞, keeps `-0`) |
| `velt_rt_math_to_int32` | `(f64) -> i32` | JS ToInt32 (bitwise operators on numbers); backends emit the case of magnitude below 2^63 inline and call it only for the rest [additive, #521] |
| `velt_rt_math_mul_int32` | `(i32 a, i32 b) -> i32` | JS `(a * b) \| 0`: the exact product rounded like the double multiply, low 32 bits; emitted inline [additive, #521] |
| `velt_rt_math_add_int32` | `(i32 a, f64 x) -> i32` | JS `(a + x) \| 0`; emitted inline, integer add when `x` is a whole number of at most 2^52 [additive, #521] |
| `velt_rt_math_clz32` | `(i32) -> i32` | `Math.clz32` of the 32-bit pattern (32 for 0); emitted as one instruction [additive, #521] |

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
  (which code not known to run outside read mode calls instead of `velt_rt_mutex_lock`) stop the
  program in read mode, since the inner callback could not tell the store's uncounted objects
  from what it changes (sema rejects the ones written in a `read` callback's body);
- a reader starts no promise or task (`velt_rt_fut_start*`, `velt_rt_spawn*` stop the program in
  read mode once the program has made an `RwLock`): what it holds would outlive the callback
  without leaving through its result;
- what the callback returns leaves by these rules too (velt_vir `async_fn/sync.rs`), before the
  unlock, whatever the calling code's mode: an object it shares with the store is copied, never
  moved out;
- the caller's containers (places rooted in a variable a `read` callback literal captured from
  code outside read mode, velt_vir `Work::ReadOwn`, also through each `read` callback around a
  nested one, `leave_levels.rs`; that code is also a function instance counting by the counts
  outside read mode, `Work::Deferred`, which a reader calls to change the caller's containers,
  and code whose mode is asked when the thread is outside read mode as the literal is
  evaluated) hold no store object and are counted by the
  counts outside read mode. What a store, a `push` or a method or function changing one
  (velt_vir `LkMode::Deferred`) keeps there gets a reference by those counts when it is a
  place the caller holds already (also a reader's variable borrowing one; what comes from the
  store sema copies), so it stays the same object; anything else is a copy of what the reader
  did not make. A value sema transferred into such a container (velt_vir leave_levels.rs
  `share_dest`) stays itself when the reader made it, by a transfer `velt_rt_xfer_share_begin`
  starts: it becomes the caller's, its parts are transferred in turn (what the reader shares
  with the value becomes a copy), and the reader's other references borrow it uncounted. A
  value the reader made that holds what the caller holds is kept by a
  transfer `velt_rt_xfer_keep_begin` starts: the caller's objects get one more reference and
  stay themselves, and what the reader made becomes the caller's. A container of the reader's
  that such a method or function may change is counted by those counts for the call and handed
  back after it, or, when its type is outside the lock set and holds lock-set objects (a
  class's `Item[]`, a `Map`), passed as a copy of what the reader did not make, dropped by
  those counts after the call. What leaves one (an element removed, the old value a store replaces, such a
  method's or function's result) loses its reference by those counts, and a lock-set object
  whose last reference goes is handed to `velt_rt_free_after_reads` with its drop glue: the
  reader may still point at it uncounted, so it is freed when the thread's outermost read ends
  (at once outside read mode; the glue releases a reference, so one stored again meanwhile
  lives on). A result the reader keeps is borrowed uncounted from then on, as the store's
  objects are, unless the reader made it (`LOCAL`).

Code whose mode is fixed counts without asking: functions only called directly from outside read
mode (and `with` / `write` callbacks, which never run in read mode) count as before; functions only called directly from a
`read` callback literal use read mode's rules. Everything else (callbacks given as values,
closures that may outlive the call, polls, glue in tables) asks `velt_rt_lk_mode` once per call.

### 9.2 How `RwLock` readers go in

The lock is *biased* (Dice and Kogan, "BRAVO: Biased Locking for Reader-Writer Locks", 2019)
until it is written: a reader names the lock in a slot of its own thread (a cache line no other
thread writes, velt_rt `hazard.rs`) and checks that no writer came, so readers on many threads
write nothing they share. A writer *revokes* the bias: it sets the lock's state and waits for the
slots naming the lock to clear, then takes the lock's underlying reader-writer lock (parking_lot's
`RawRwLock`) exclusively. While it waits the state says so (revoking): one writer revokes at a
time, and another writer, holding the underlying lock or not, writes only once the state says
the bias is revoked, and waits for the revocation without holding the lock. Each change of the
state has one owner (biased to revoking: the revoker's compare-exchange; revoking to revoked:
that revoker; revoked to biased: a reader's compare-exchange), so a revoker held up for any
time never takes a later revocation for its own. It never waits for the slots while holding the
underlying lock: a
reader in a slot may be nested in a read of another lock whose writer waits for this writer's
readers. A reader that biased the lock again before the writer took it (its revocation was old
enough) makes the writer let the underlying lock go and revoke once more. From then on readers take the underlying lock shared, so they still
run at once and a lock written often costs about what a `Mutex` does, until a reader holding it
finds the revocation old enough and biases the lock again. A writer blocked on the underlying
lock holds new outermost readers back, so writers don't starve, while a read nested in another
goes past it (a recursive shared lock), so readers nesting reads of two locks in opposite orders
never wait for each other's writers. How long a revocation lasts adapts to how long the
bias before it lasted (at most 10 ms). The lock words are the underlying lock, then (on the next
128-byte line, which only revocations write) the state.

A thread waiting for the underlying lock backs off exponentially between tries (about 0.5 µs
doubling to 35 µs, yielding each time, blocking after about 0.7 ms): a waiter that tries takes
the line from the holder, so short callbacks taking turns run several in a row on one core.
Generated code passes each lock call the reader state an earlier one in the same call of the
function returned, so the common reads and writes run without a
thread-local lookup or a call.

## 10. Output with many tasks

`console.log` from tasks uses per-thread stdout buffers; the runtime publishes a thread's buffer
into the shared stdout buffer after every task poll, so output order follows causality
(`log a; await x; log b` prints a before b even if the task moved threads) and lines from
different tasks never interleave mid-line (a full thread buffer is published only through its last
newline; only a single line longer than 1 MiB may be split). Buffered output reaches the OS when buffers fill, when
a worker goes idle (so servers logging to a pipe show output promptly), before stderr writes, on
`velt_rt_flush`, and at exit.

## 11. Worked example (what lowering emits)

```c
// async function add1After(a: i64, ms: i64): Promise<i64> { await sleep(ms); return a + 1; }
typedef struct { int64_t result; uint32_t tag; int64_t a, ms; VeltFut* sleep; } Add1After;
uint32_t add1After$poll(void* s, void* cx) {
    Add1After* st = s;
    switch (st->tag) {
    case 0: st->sleep = velt_rt_sleep(st->ms); st->tag = 1; /* fallthrough */
    case 1: if (!velt_rt_fut_poll(st->sleep, cx)) return 0;
            velt_rt_fut_drop(st->sleep);
            st->result = st->a + 1; st->tag = 2; return 1;
    }
}
void add1After$drop(void* s) { Add1After* st = s; if (st->tag == 1) velt_rt_fut_drop(st->sleep); }

// const h = spawn(add1After(41, 5)); ... await h
Add1After init = { 0, 0, 41, 5, 0 };
st->h = velt_rt_spawn(add1After$poll, add1After$drop, &init, sizeof init, 8, sizeof(int64_t), NULL);
... if (!velt_rt_fut_poll(st->h, cx)) return 0;
    int64_t v = *(int64_t*)((char*)st->h + 16); velt_rt_fut_drop(st->h);

// async function main() { ... }   →   int32_t velt_main(void) {
//     Main m = {0}; velt_rt_block_on(main$poll, &m); return 0; }
```

## 12. M2/M4 strings & JSON

Status: **proposal by the runtime agent, implemented in `crates/velt_rt`** (`strbuf.rs`, `str_ops/`,
`json/`; tested from Rust against node-generated tables in `tests/abi/`, and from C in
`tests/link_check.rs`). Not yet frozen. All strings are UTF-8 (`VeltStr`, forms in rt_abi.md).

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

**Invariant:** a count-1 buffer is appended to or adopted (and so possibly reallocated, and for
an adoption its text moved within it) only while no borrowed static-form view into it is live.
The only such views today are the JSON reader's borrowed keys (§12.3), which point into the
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
| `velt_rt_strbuf_push_i64` / `_u64` | `(VeltStrBuf* b, i64 / u64 v)` | decimal |
| `velt_rt_strbuf_push_f64` | `(VeltStrBuf* b, f64 v)` | JS `String(v)` (`-0` is `0`) |
| `velt_rt_strbuf_push_inspect_f64` | `(VeltStrBuf* b, f64 v)` | what `console.log` prints, as `velt_rt_write_f64` (`-0` is `-0`) [additive, #525] |
| `velt_rt_strbuf_push_json_f64` | `(VeltStrBuf* b, f64 v)` | like `JSON.stringify`: JS format, `null` for NaN/±Infinity |
| `velt_rt_strbuf_push_inspect_str` | `(VeltStrBuf* b, const VeltStr* s)` | a string as `console.log` shows it inside a container (node `util.inspect` quoting and escaping) |
| `velt_rt_strbuf_push_inspect_key` | `(VeltStrBuf* b, const VeltStr* s)` | an object key as `console.log` shows it: bare if it matches `[A-Za-z_][A-Za-z0-9_]*` (node quotes `$`), else quoted like `push_inspect_str` |
| `velt_rt_strbuf_push_bool` | `(VeltStrBuf* b, u8 v)` | `true`/`false` |
| `velt_rt_strbuf_push_byte` | `(VeltStrBuf* b, u8 c)` | punctuation in generated glue |
| `velt_rt_strbuf_push_json_str` | `(VeltStrBuf* b, const VeltStr* s)` | quoted + escaped exactly like `JSON.stringify(s)`: `\"` `\\` `\b \f \n \r \t`, other controls < U+0020 as lowercase 6-char `\u00xx`; everything else verbatim |
| `velt_rt_strbuf_push_json_value` | `(VeltStrBuf* b, const void* v)` | `JSON.stringify(v)` of a `json.Value` field (null handle ⇒ `null`); emitted by the compiler, which passes the handle's address as a pointer (VIR `ptr`), unlike the `u64` handles of §3.2 |
| `velt_rt_strbuf_push_inspect_json` | `(VeltStrBuf* b, const void* v, u8 top, u32 depth)` | what `console.log` prints for a `json.Value` (node `util.inspect` of the parsed value: `{ a: 1, b: [ 2, 'x' ] }`, `[]`, `{}`, strings quoted like `push_inspect_str`, keys like `push_inspect_key`); a string is raw when `top != 0`, and an array or object is then broken across lines like `inspect_layout`; null handle ⇒ `null`. `depth` is node's depth of the value (0 for a `console.log` argument): a non-empty array or object nested deeper than 2 prints as `[Array]` / `[Object]`, and an array shows its first 100 elements, then `... n more items` (node's `depth` and `maxArrayLength`). The handle is passed like `push_json_value`'s (additive; `depth` added by #497) |
| `velt_rt_strbuf_inspect_begin` | `()` | start of a top-level value printed by `console.log`, `${x}` or `String(x)`: the `<ref *N>` numbering of cycles starts over (node numbers per argument), unless an object is being printed (additive) |
| `velt_rt_strbuf_inspect_enter` | `(VeltStrBuf* b, const void* p) -> u8` | start printing the object at `p` (class instance or recursive object): `1`, or, when `p` is already being printed (a cycle), append `[Circular *N]` and return `0` (the caller skips it); `N` is the object's number for the whole top-level value (additive) |
| `velt_rt_strbuf_inspect_leave` | `(VeltStrBuf* b)` | done with the innermost entered object; an object with a number gets the `<ref *N> ` prefix at the start of its text (additive) |
| `velt_rt_strbuf_inspect_circular` | `(VeltStrBuf* b, const void* p) -> u8` | for an object past node's depth limit: when `p` is being printed, append `[Circular *N]` (numbered like `inspect_enter`) and return `1`; else `0` (the caller prints `[Name]`). The object is not entered (additive) |
| `velt_rt_strbuf_inspect_atom` | `(const VeltStrBuf* b, u64 start)` | the text from byte `start` to the end of `b` is what a class's `__inspect()` returned: `inspect_layout` keeps it as one piece (node inserts a custom inspect's string as it is). Forgotten at the next `inspect_begin` (additive) |
| `velt_rt_strbuf_inspect_more` | `(VeltStrBuf* b, u64 remaining)` | append node's `, ... n more items` (`, ... 1 more item`) after the first 100 entries of an array, `Map` or `Set` (additive) |
| `velt_rt_strbuf_len` | `(const VeltStrBuf* b) -> u64` | byte length: where the next value's text starts (for `inspect_layout`) |
| `velt_rt_strbuf_inspect_layout` | `(VeltStrBuf* b, u64 start)` | re-lays out the text of one printed value, from byte `start` to the end, the way node's `util.inspect` breaks it across lines (`breakLength` 80, 2-space indentation, arrays of more than six short entries in columns (an array's `... n more items` entry on its own line after them), long strings split at line breaks); a value of at most 71 bytes with fewer than six commas and no line break is left alone without being parsed; a custom inspect's text (`velt_rt_strbuf_inspect_atom`) is kept as it is, its lines after the first indented by the value's indentation, as node indents a custom inspect's string. Emitted after each top-level `console.log` / `${}` value that can hold containers |
| `velt_rt_strbuf_finish` | `(VeltStrBuf* b, VeltStr* out)` | moves the text to `*out`; `*b` becomes empty (reusable, nothing to free) |
| `velt_rt_strbuf_drop` | `(VeltStrBuf* b)` | abandon an unfinished builder (exception path); zeroes it |
| `velt_rt_json_enter` | `(const void* p) -> u8` | `JSON.stringify` glue starts writing the object at `p`: 1, or 0 if it is already being written (a cycle; the caller panics with `JSON.stringify: converting circular structure to JSON`). Per thread |
| `velt_rt_rows_begin` / `_end` | `(const RawCursor* c, const u8* fields) -> RowsState*` / `(RowsState* st)` | `Glue::RowsRead` (std's `__intrinsic_json_parse_cursor<T>` when the row type is an object of plain fields) reads a driver's row cursor by column: `fields` is static data (`u32` count, then per field `u32` byte length and the name); each column is matched to its field once (the map is kept per thread for the next result with the same header and table). Additive |
| `velt_rt_rows_next` / `_columns` / `_field` | `(st) -> u8` / `(st) -> u64` / `(st, u64 col) -> i64` | step to the next row (1, or 0 at the end); the column count; the field column `col` fills (-1: none). Additive |
| `velt_rt_rows_i64` / `_f64` / `_bool` / `_str` / `_bytes` | `(st, u64 col, T* out) -> u8` | column `col` of the current row typed: 1 = written, 2 = NULL, 0 = another kind (an integer accepts an exact float, a float an integer, a string's invalid UTF-8 becomes U+FFFD). Additive |
| `velt_rt_rows_map` / `_map_column` | `(const u8* fields) -> RowsState*` / `(RowsState* st, const VeltStr* name)` | the column map of std/db' `RowSource` written in Velt (`__intrinsic_read_rows<T, S>`), whose rows the compiled code reads through the source's own methods: a state without a cursor, then each column's name in order. Additive |
| `velt_rt_rows_exact_i64` | `(f64 v, i64* out) -> u8` | 1 and the integer when `v` is one exactly (an integer field from a float cell), else 0. Additive |
| `velt_rt_rows_fail` | `(const VeltStr* expected, i64 row, const VeltStr* field, VeltStr* out)` | the JSON decoder's message for a row that does not fit: `expected <expected> at $[row].field` (`row` -1: the single row; `field` empty: the row itself). Additive |
| `velt_rt_param_object` / `velt_rt_param_name` | `(const RawSink* s)` / `(const RawSink* s, const VeltStr* name)` | `Glue::DbBind` (`__intrinsic_db_bind<P>`, std only) hands a driver's parameter sink (native_abi.md "Parameter sinks") an object's members: `object` first, then each member's name before its value. Additive |
| `velt_rt_param_i64` / `_u64` / `_f64` / `_bool` / `_null` / `_str` / `_bytes` | `(const RawSink* s, v)` (`_str`: `const VeltStr*`, `_bytes`: `const VeltBytes*`, `_null`: no value) | one value, as its JSON text would bind: a `u64` beyond `i64` and a non-integral `f64` (or one of 1e21 or more) a REAL, an integral `f64` an INTEGER, NaN/±Infinity NULL, a string UTF-8 (lone surrogates U+FFFD), a `u8[]` a BLOB. Additive |
| `velt_rt_json_leave` | `()` | done with the innermost object entered |

### 12.2 String methods

**Indexing model** (#377 phase 2b, design/strings.md): indexes and lengths are **UTF-16 code
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

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_slice` | `(const VeltStr* s, i64 start, i64 end, VeltStr* out)` | JS `slice`: negative = from end, clamped, empty if `start >= end`; omitted `end` = `i64::MAX` |
| `velt_rt_str_index_of` | `(const VeltStr* s, const VeltStr* needle, i64 from) -> i64` | `from` clamped to `0..=len` (omitted = 0); -1 if absent; empty needle ⇒ clamped `from` |
| `velt_rt_str_last_index_of` | `(const VeltStr* s, const VeltStr* needle, i64 from) -> i64` | last match starting `<= from`; omitted `from` = `i64::MAX` |
| `velt_rt_str_includes` | `(const VeltStr* s, const VeltStr* needle) -> u8` | (a `position` arg ⇒ use `index_of(s, n, pos) >= 0`) |
| `velt_rt_str_starts_with` / `_ends_with` | `(const VeltStr* s, const VeltStr* affix) -> u8` | |
| `velt_rt_str_eq` | `(const VeltStr* a, const VeltStr* b) -> u8` | `==` fast path: length check + one memcmp |
| `velt_rt_str_join` | `(const VeltStrArray* parts, const VeltStr* sep, VeltStr* out)` | `parts.join(sep)`: sums the pieces' lengths and unit counts, then writes the result once (inline, or a heap buffer of exactly its size) |
| `velt_rt_str_split` | `(const VeltStr* s, const VeltStr* sep, VeltStrArray* out)` | JS semantics: `"a,b,".split(",")` = `["a","b",""]`, `"".split(",")` = `[""]`, `"".split("")` = `[]`, `split("")` = code units (a pair gives its two halves). Drop with `velt_rt_str_array_drop` (§4). |
| `velt_rt_str_code_points` | `(const VeltStr* s, VeltStrArray* out)` | the characters (code points; a pair is one, a lone surrogate one): what `for (const c of s)` and `[...s]` iterate (the prelude's `__codePoints`) |
| `velt_rt_str_is_well_formed` | `(const VeltStr* s) -> u8` | 1 when `s` has no lone surrogates (std's `encodeURIComponent`) |
| `velt_rt_str_trim` / `_trim_start` / `_trim_end` | `(const VeltStr* s, VeltStr* out)` | JS WhiteSpace + LineTerminator set (includes U+FEFF, U+00A0, U+2028/9, Zs; not U+0085) |
| `velt_rt_str_to_upper` / `_to_lower` | `(const VeltStr* s, VeltStr* out)` | full Unicode default case mapping (`ß` → `SS`, final sigma), like JS; ASCII fast path |
| `velt_rt_str_replace` | `(const VeltStr* s, const VeltStr* from, const VeltStr* to, VeltStr* out)` | first occurrence; `to` expands JS patterns `$$`, `$&`, `` $` ``, `$'` (others literal) |
| `velt_rt_str_replace_all` | same | all non-overlapping occurrences; empty `from` inserts at every character boundary |
| `velt_rt_str_repeat` | `(const VeltStr* s, i64 n, VeltStr* out) -> u8` | 1 = ok; 0 = JS `RangeError` (n < 0 or result too large), `*out` empty — the compiler throws/panics |
| `velt_rt_str_pad_start` / `_pad_end` | `(const VeltStr* s, i64 target, const VeltStr* fill, VeltStr* out)` | lengths in code units; the last partial `fill` is cut to the units missing, between the halves of a pair if need be (as JS); omitted `fill` = `" "` |
| `velt_rt_str_char_code_at` | `(const VeltStr* s, i64 i) -> i64` | the UTF-16 code unit at `i`; -1 if out of range (JS: NaN). Generated code reads ASCII strings inline (rt_abi.md) |
| `velt_rt_str_from_char_code` | `(i64 code, VeltStr* out)` | JS `ToUint16(code)`; a surrogate gives a lone surrogate, as JS; ASCII results are static (no allocation) |
| `velt_rt_parse_int` | `(const VeltStr* s, i64 radix) -> f64` | exact JS `parseInt`: leading JS whitespace, sign, `0x` prefix (radix 0/16), radix `ToInt32`, 0 = omitted, outside 2..36 ⇒ NaN, longest digit prefix, none ⇒ NaN, `-0` kept. Correctly rounded for radix 10 and powers of two; other radixes exact below 2^128 (V8 is not correctly rounded there either). |
| `velt_rt_parse_float` | `(const VeltStr* s) -> f64` | exact JS `parseFloat`: leading whitespace, longest `StrDecimalLiteral` prefix (incl. `Infinity`), else NaN |
| `velt_rt_str_to_number` | `(const VeltStr* s) -> f64` | exact JS `Number(s)`: trimmed; `""` ⇒ 0; `0x`/`0o`/`0b` (unsigned); `±Infinity`; whole string must be a decimal literal, else NaN |
| `velt_rt_str_locale_compare` | `(const VeltStr* s, const VeltStr* t) -> i64` | `s.localeCompare(t)`: -1/0/1 in the CLDR root collation (`Intl.Collator("und")`); exact for the blocks in the table generated by `scripts/gen-collation-table.js` (U+0020..U+024F, U+0370..U+04FF, U+1E00..U+1EFF, U+2000..U+206F and U+20A0..U+20CF) except characters that expand to three or more elements (`¼`, `ϗ`), approximate elsewhere (additive) |
| `velt_rt_str_array_drop` | `(VeltStrArray* a)` | already in §4 |

### 12.3 JSON pull reader (`JSON.parse<T>`)

The compiler generates one decoder per target type; it drives a reader over the borrowed source.
Lexing is byte-level; nothing is allocated except the owned strings handed out (and keys/strings
that contain escapes).

```c
typedef struct VeltJsonReader VeltJsonReader;   // opaque
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_json_reader_new` | `(const VeltStr* src) -> VeltJsonReader*` | never null; `src` must stay alive, unchanged **and in place** (an inline string's bytes live in the `VeltStr` itself) until `free` |
| `velt_rt_json_reader_new_with` | `(const VeltStr* src, u32 flags, u32 max_depth) -> VeltJsonReader*` (with flag 4, `src` is a `const VeltBytes*`) | `JSON.parse` options: flag 1 = `skip_unknown` fails; flag 2 = a JSON `null` for an optional field without `null` is not an absent field (`velt_rt_json_reader_null_absent`); `max_depth` = most arrays/objects open at once, counted from the top-level value (0 = no limit), checked by `expect_*_start`, `skip_value` and `read_value`. Flag 4 (`FLAG_ROWS`, set only by `__intrinsic_json_parse_rows`): `src` is a `const VeltBytes*`, a `u8[]` of a driver's binary rows (`velt_text::rows`, native_abi.md "Rows"), which the reader walks instead of text, borrowed like a source string. Every reader call then answers as it would for the JSON the rows stand for; the only failures are a value of another kind and a rejected unknown key. `max_depth` does not apply to such rows (they nest three deep at most). Flag 8 (`FLAG_CURSOR`, set only by `__intrinsic_json_parse_cursor`, additive): `src` is a driver's row cursor (`const RawCursor*`, native_abi.md "Row cursors"), read as binary rows are, which the reader uses but does not free; a decoder that marks a position outside a row (a union of whole results) gets the rows left copied into binary form |
| `velt_rt_json_reader_free` | `(VeltJsonReader* r)` | null ok |
| `velt_rt_json_reader_null_absent` | `(VeltJsonReader* r) -> u8` | 1 when a JSON `null` may stand for an absent optional field whose type has no `null` (`JSON.parse<T>`), 0 when the reader was made with flag 2 (`JSON.parse(text) as T`): the decoder then reads the field's value, which fails |
| `velt_rt_json_reader_peek` | `(VeltJsonReader* r) -> u32` | next token kind, skipping whitespace: 0 EOF, 1 `null`, 2 `true`, 3 `false`, 4 number, 5 string, 6 `[`, 7 `]`, 8 `{`, 9 `}`, 10 error (bad byte, or the reader already failed). Classifies by first byte only; the `read_*` call validates. |
| `velt_rt_json_reader_expect_object_start` | `(r) -> u8` | consume `{` |
| `velt_rt_json_reader_next_key` | `(r, VeltStr* out) -> u8` | **1** = `*out` is the next key and its `:` is consumed (read the value next); **0** = `}` consumed (end of object); **2** = error. Handles the commas. The key **borrows** the source (static form) unless it had escapes or is a non-ASCII key of more than 64 UTF-16 units (copied: threads remember positions in long static strings, `str/recent.rs`, which is sound only for bytes never freed) — compare it, don't keep it (`velt_rt_str_own` makes a copy to keep: `velt_rt_str_clone` keeps the borrow); dropping it is always allowed. |
| `velt_rt_json_reader_expect_array_start` | `(r) -> u8` | consume `[` |
| `velt_rt_json_reader_array_next` | `(r) -> u8` | **1** = another element follows (read it next), **0** = `]` consumed, **2** = error |
| `velt_rt_json_reader_read_string` | `(r, VeltStr* out) -> u8` | owned, decoded (`\u` escapes are code units: an escaped pair is one character, a lone surrogate stays lone and joins a raw half next to it) |
| `velt_rt_json_reader_read_f64` | `(r, f64* out) -> u8` | correctly rounded like `JSON.parse` (`-0` kept, `1e400` → Infinity) |
| `velt_rt_json_reader_read_i64` | `(r, i64* out) -> u8` | the number must be an exact integer in i64 range (`3`, `3.0`, `3e2` ok; `2.5`, `1e400`, `9223372036854775808` fail as mismatch). Digit strings are converted exactly (beyond 2^53). |
| `velt_rt_json_reader_read_bool` | `(r, u8* out) -> u8` | |
| `velt_rt_json_reader_read_null` | `(r) -> u8` | optional fields: `if (peek(r) == 1) read_null(r); else read the T` |
| `velt_rt_json_reader_skip_unknown` | `(r) -> u8` | the value of an object key the target type has no field for: `skip_value`, or a failure when the reader rejects unknown keys |
| `velt_rt_json_reader_skip_value` | `(r) -> u8` | skips (and validates) any value — unknown keys; iterative (no recursion), but nesting past the reader's `max_depth` fails like `expect_*_start` |
| `velt_rt_json_reader_skip_lookahead` | `(r) -> u8` | `skip_value` for a union decoder looking ahead for its discriminant: also remembers where each array/object it passes ends (by the offset of its opening bracket), and jumps over one already passed. So the lookahead of unions nested in the skipped value does not scan it again: nested unions stay linear in the input size. Memory: one entry per container skipped this way, freed with the reader |
| `velt_rt_json_reader_read_value` | `(r, VeltJson* out) -> u8` | any one value as a `json.Value` tree (§12.5), an owned handle: a typed decoder's `JsonValue` target. Iterative (no recursion); nesting past the reader's `max_depth` fails like `expect_*_start`. |
| `velt_rt_json_reader_mark` | `(const VeltJsonReader* r) -> u64` | the current position (opaque), for `reset` |
| `velt_rt_json_reader_reset` | `(r, u64 mark)` | go back to a `mark` of the same reader and clear any error since: union decoders look ahead (for a discriminant key, or a number against literal members) and then decode from the start of the value |
| `velt_rt_json_reader_end` | `(r) -> u8` | after the top-level value: 1 if only whitespace remains |
| `velt_rt_json_error` | `(const VeltJsonReader* r, const VeltStr* expected, const VeltStr* path, VeltStr* out)` | builds the owned `JsonError.message` (§12.4) |

Except where stated, `u8` results are 1 = ok, 0 = failed (outputs untouched). **The first failure
is sticky**: every later call fails (`peek` returns 10), so a decoder can bail out on the first 0
and call `velt_rt_json_error` with what it wanted and where. JSON is strict RFC 8259 (no trailing
commas, no leading zeros / `+` / `.5` / `NaN`, no raw control characters in strings). Duplicate keys
are simply reported twice (the decoder's last assignment wins, like `JSON.parse`).

Decoder shape for `struct User { name: string; age: i64; tags: string[]; email?: string; }`:
```c
if (!expect_object_start(r)) return fail(r, "object", "$");
for (;;) {
    uint8_t k = next_key(r, &key);
    if (k == 0) break;
    if (k == 2) return fail(r, "object", "$");
    if (str_eq(&key, "name"))      { if (!read_string(r, &u->name)) return fail(r, "string", "$.name"); }
    else if (str_eq(&key, "age"))  { if (!read_i64(r, &u->age)) return fail(r, "i64", "$.age"); }
    else if (str_eq(&key, "tags")) { expect_array_start … while (array_next(r) == 1) read_string … "$.tags[i]" }
    else if (str_eq(&key, "email")) { if (peek(r) == 1) read_null(r); else if (!read_string(r, &s)) … }
    else if (!skip_value(r)) return fail(r, "value", "$.<key>");
    str_drop(&key);
}
/* missing required field: */ fail(r, "field \"age\"", "$");
/* root decoder only: */ if (!end(r)) return fail(r, "end of input", "$");
```
Paths only need to be built on the failure path (e.g. append segments while returning).

### 12.4 JSON error messages

| Situation | Message |
|---|---|
| well-formed, but a different kind than the decoder asked for (incl. non-integral / out-of-range for `read_i64`), or no reader error at all (e.g. missing field) | `expected <expected> at <path>` — e.g. `expected string at $.name` (golden) |
| the text is not JSON (anywhere in it, also past the point where the decoder stopped; `JSON.parseValue` too) | Node 22's `JSON.parse` message, e.g. `Expected property name or '}' in JSON at position 1 (line 1 column 2)`, `Unexpected end of JSON input`, `Unexpected token 'x', "[x]" is not valid JSON` (positions, lines and columns in UTF-16 units) |
| syntax error the reader sees that V8 accepts (none is known) | `invalid JSON at <path>: <detail> (byte <offset>)`; for `parseValue` the path is that of the value being read when the error occurred (`$[1]`, `$.a`) |

`velt_rt_json_error` checks the whole text for a Node syntax error first (only on this failure
path, so it costs nothing on success), then gives the decoder's message.

With the reader's options: an unknown key (rejected) is `unknown field at <path>` (the decoder
puts the key in the path), and nesting past `max_depth` is
`JSON nested deeper than <max_depth> levels at <path> (byte <offset>)` (also from
`velt_rt_json_parse_value_with`). The prelude passes `max_depth` 128 unless the program sets
`maxDepth`.

`<detail>` is one of `unexpected end of input`, `unexpected character 'c'` (control characters as
`U+XXXX`), `expected ':'`, `expected ',' or '}'`, `expected ',' or ']'`, `expected string key`,
`invalid number`, `invalid escape`, `invalid \u escape`, `control character in string`,
`unexpected trailing characters`. `<offset>` is the byte offset in the source's UTF-8 (not a
string position: it locates the error in the input text as an editor or a byte-level tool sees
it; for ASCII input the two agree). Path syntax
(`$`, `$.a.b`, `$.tags[1]`) is the compiler's choice; the runtime inserts it, shortened when it
has more than 20 segments (each starting at `.` or `[`) to the first and last 10 with `…`
between (`$[0][0]…[0].name`).

### 12.5 `json.Value` (`JSON.parseValue`)

A tree of reference-counted nodes behind opaque handles (`VeltJson`), with value semantics:
the editors below copy a node that another handle shares before changing it. Every
handle the runtime returns (`parse_value`, `get`, `at`, `clone`, `reader_read_value`, `new_*`) is its own reference and must be
released with `velt_rt_json_value_free`; a child handle stays valid after its parent's handles are
freed. All accessors accept a null handle (what a failed `get`/`at` returns), so `v.get("a")?.at(2)`
chains need no checks until the end. Parsing, stringify and freeing are iterative, so deep nesting
cannot overflow the stack (parsing stops at `max_depth` when one is given).
Objects keep key order (first occurrence; a duplicate key replaces the value, like `JSON.parse`);
objects with more than 16 keys get a hash index for `get`.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_json_parse_value` | `(const VeltStr* src, VeltJson* out, VeltStr* out_err) -> u8` | 1 = ok; 0 = syntax error: `*out` = 0, `*out_err` = owned message (§12.4). No depth limit (`JsonValue.from`) |
| `velt_rt_json_parse_value_with` | `(const VeltStr* src, u32 max_depth, VeltJson* out, VeltStr* out_err) -> u8` | the same, failing on arrays/objects nested more than `max_depth` deep (0 = no limit) with the `max_depth` message of §12.4: `JSON.parseValue(text, { maxDepth })`, default 128 (set by the prelude) |
| `velt_rt_json_value_kind` | `(VeltJson v) -> u32` | 0 none (null handle), 1 null, 2 bool, 3 number, 4 string, 5 array, 6 object |
| `velt_rt_json_value_get` | `(VeltJson v, const VeltStr* key) -> VeltJson` | member, or null (not an object / missing) |
| `velt_rt_json_value_at` | `(VeltJson v, u64 i) -> VeltJson` | array element, or the i-th member value of an object; null if out of range (including an `i` beyond `usize` on 32-bit targets) |
| `velt_rt_json_value_key_at` | `(VeltJson v, u64 i, VeltStr* out) -> u8` | i-th object key (owned); 0 if not an object / out of range (as for `at`) |
| `velt_rt_json_value_len` | `(VeltJson v) -> u64` | array length, object member count, string length (UTF-16 code units); else 0 |
| `velt_rt_json_value_as_f64` | `(VeltJson v) -> f64` | NaN if not a number |
| `velt_rt_json_value_as_bool` | `(VeltJson v) -> u8` | 1 only for `true` (use `kind` to tell `false` from non-bools) |
| `velt_rt_json_value_as_str` | `(VeltJson v, VeltStr* out) -> u8` | owned copy; 0 if not a string |
| `velt_rt_json_value_stringify` | `(VeltJson v, VeltStr* out)` | `JSON.stringify`: no whitespace, key order kept, numbers JS-formatted; null handle ⇒ `null` |
| `velt_rt_json_relayout` | `(const VeltStr* text, const VeltStr* keys, u8 flags, const VeltStr* indent, VeltStr* out)` | `JSON.stringify(value, replacer, space)` from the compact `text` of `JSON.stringify(value)`: with `flags` bit 0, objects at any depth keep only the keys listed in `keys` (the compact JSON text of an array of strings; duplicates ignored), in that order; with bit 1, line breaks, `indent` (JS's gap, possibly empty, as for a `space` in (0, 1)) per level and `": "` after keys; without it no whitespace (`indent` ignored); malformed `text` is returned unchanged |
| `velt_rt_json_value_clone` | `(VeltJson v) -> VeltJson` | O(1) new reference to the same value (may be the same pointer); the editors copy a node shared this way before changing it, so neither handle sees the other's later edits |
| `velt_rt_json_value_free` | `(VeltJson v)` | null ok |
| `velt_rt_json_value_new_null` / `_new_bool(u8)` / `_new_number(f64)` / `_new_string(const VeltStr*)` / `_new_array()` / `_new_object()` | `(…) -> VeltJson` | a new value (the string is copied) |
| `velt_rt_json_value_set` | `(VeltJson* slot, const VeltStr* key, VeltJson v) -> u8` | object member `key` = `v` (an existing key keeps its position); 0 if `*slot` is not an object. `v` is shared, not consumed (null handle = JSON `null`). `*slot` may be replaced by a copy (copy-on-write); the old handle's reference is released then |
| `velt_rt_json_value_delete` | `(VeltJson* slot, const VeltStr* key) -> u8` | remove member `key`, keeping the order of the others; 0 if absent or not an object. O(1) amortized: an object with an index leaves a hole (compacted once holes outnumber members). `len` stays O(1), and `at`/`key_at` stay O(1) while the holes are only at the ends; with holes in the middle they cost O(log n) through a Fenwick tree of the live slots, built in O(n) by the first such access after a compaction and updated in O(log n) by each later delete and insert |
| `velt_rt_json_value_push` | `(VeltJson* slot, VeltJson v) -> u8` | append to an array; 0 if not an array |
| `velt_rt_json_value_set_at` | `(VeltJson* slot, u64 i, VeltJson v) -> u8` | replace element `i`; 0 if not an array or out of range (as for `at`: never truncated to a smaller index) |

`isNull()` = `kind == 1`, `asNumber()` = `as_f64` after checking `kind == 3`, etc.

### 12.6 Performance (release, x86_64 Windows, `cargo test -p velt_rt --release --lib text_perf -- --nocapture`)

- Builder: 1M `push_str` + 1M `push_i64` into one string ≈ 12 ms.
- 10.5 MB array of 75k objects (escapes, nested unknown keys): `skip_value` ≈ 415 MB/s, a
  generated-style typed decoder ≈ 230 MB/s, `JSON.parseValue` + free ≈ 77 MB/s.

## 13. Dev mode (`velt dev`, docs/internals/design/hot-reload.md phases 1–3)

Nothing here changes the ABI generated code uses; it is how the runtime behaves under the
`velt dev` supervisor, and how `velt` hosts JIT-compiled programs.

### 13.1 Listener handover
`VELT_DEV_SOCKET` names the supervisor's dev channel: a Unix socket path, or on Windows a local
named pipe (`\\.\pipe\velt-dev-<pid>-<n>`, byte mode; a program retries for up to 5 s while every
pipe instance is busy). Each request is one connection. When the variable is set,
`velt_rt_http_serve` and `velt_rt_tcp_listen` do not bind themselves: they connect, send
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
its port. Without the variable the runtime binds as before.

### 13.2 Stopping
In dev mode (`VELT_DEV_SOCKET` set, once the program listens) the runtime handles a stop request:
HTTP servers stop accepting (the supervisor still holds the socket), in-flight requests get up to
1 s to finish, stdout is flushed and the process exits with status 0. The stop request is:
- Unix: SIGTERM. Outside dev mode SIGTERM keeps its default action.
- Windows (no SIGTERM): before its first `listen` request the program opens a **stop channel**, a
  connection that sends `watch-stop\n` and gets `ok\n`; the supervisor keeps it (by the client
  process id) and writes `stop\n` to ask for the stop. The supervisor closing the channel (it
  exited) also counts as a stop request. A program without a stop channel (it never listened) is
  terminated, like SIGTERM's default action. A Unix supervisor answers `watch-stop` with `err`.

The supervisor kills a program that has not exited 1.5 s after the stop request.

### 13.3 Build report (JIT host)
One more request on the same dev channel, sent by `velt dev --host` (not by programs): `built ok\n` or
`built failed\n`, then one line per source file the build read, then an empty line. After
`ok` the host blocks until the supervisor answers `go\n` (sent once the previous version has
stopped), then runs the program.

**Reload channel** (hot swap, docs/internals/design/hot-reload.md phase 3): after `go` the host keeps
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
`[dependencies]` must equal `velt_rt`'s (checked by a test). It exposes:
- `abi_symbols::ABI_SYMBOLS: [(&str, AbiAddress); N]`, generated by `crates/velt_rt/build.rs` from
  every `#[no_mangle] extern "C"` function in the sources (except `main`); the JIT registers it
  with `JITBuilder::symbol`. A test checks it contains every `velt_rt_*` name in rt_abi.md and
  this file. Other symbols (`memcpy`, `fmod`, ...) resolve from the process.
- `entry::run_main(velt_main)`: what `main` does (runtime init, call, flush stdout) → exit code.
- `process::set_args(argv)`: `argv()` / `args()` for the hosted program (`velt`'s own arguments
  are not the program's).
- `process::set_script(path)`: the script of Node's `process.argv[1]` for the hosted program
  (`velt dev`); otherwise the runtime reads `$VELT_SCRIPT` once at start-up and removes it.

On Windows x64 the JIT (`velt_codegen_cl::DevSession`) registers each loaded program's unwind
info with `RtlAddFunctionTable` (code and records in one arena), so stack walks get through JIT
frames as they do through linked code's `.pdata`.

### 13.5 Handler re-read and the rule against cached code pointers
In dev mode an HTTP server re-reads its `VeltHandler` for every request (an atomic pointer) and
keeps handler states on the heap, so `velt_rt::http::handler::replace_handler(index, desc)` (Rust
API for the phase 3 hot swap; `index` = `serve` order) gives new requests new code while requests
in flight finish on theirs (their future copied `poll`/`drop`). Replaced descriptors are never
freed. Outside dev mode the descriptor is read once, as before.
`velt_rt::http::handler::update_handlers(f)` offers every running server's descriptor to `f`
and installs the replacement it returns: after a hot swap that recompiled a handler, the host
replaces `init`, `poll`, `drop` and the state size/alignment with the new version's (they share
one state layout, so all of them change together; `env` stays). In dev builds the descriptor's
`init` is that version's code itself, not a trampoline.
Rule: only vtables (via relocations), `VeltFut` headers and these per-server handler slots may
store code addresses. New runtime APIs that take callbacks (timers, WebSockets, child processes,
...) must keep them replaceable the same way. The one exception is drop glue kept with a value in
flight (a join's or a spawned task's result, a channel item being sent) or with the values a
channel holds (the `item_drop` a channel keeps from its sends, §2.2, called only at program end): it
matches the values' layout, which a swap doesn't change (a changed layout restarts the program),
and old code is never freed, so the pointer stays callable after a swap. A value's drop glue goes
away with the value; a channel's lives as long as the channel.


## 14. Standard library breadth (stream std-net; additive)

New runtime symbols behind the std modules of docs/std/README.md. Same conventions as above: string
arguments are borrowed `const VeltStr*`, `out` parameters receive owned values, handles are
opaque pointers (`u64` in Velt). None of them store code pointers (§13.5): callbacks in these
modules are compiled Velt code (timers are tasks), never function pointers kept by the runtime.

`VeltArray<T>` = `{ T* ptr; u64 len; u64 cap; }` (cap in elements), the layout of a Velt `T[]`
of a Copy element type, built from a Rust `Vec<T>` (same allocator), like `VeltStrArray`.

### 14.1 Randomness and local time

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_random_bytes` | `(u64 n, VeltBytes* out)` | `n` bytes from the OS CSPRNG (`getrandom`); failure of the OS generator is fatal |
| `velt_rt_random_u64` | `() -> u64` | uniform, OS CSPRNG |
| `velt_rt_local_offset_minutes` | `(i64 epoch_ms) -> i32` | minutes to add to UTC for local time at that instant (DST-aware: the C library tz database on Unix; on Windows `TZ` when it names UTC or a fixed offset (`Etc/GMT±N`), else the system time zone with its dynamic per-year DST rules, `SystemTimeToTzSpecificLocalTimeEx`) |

### 14.2 Regular expressions (`velt:regex`)

`VeltRegex` is an `Arc` of a compiled Rust `regex::bytes::Regex`; a JS pattern is rewritten
first (ASCII `\d \w \b`, JS class literals; `crates/velt_rt/src/regex/syntax.rs`). Matching runs
on the subject's WTF-8 bytes; the offsets taken (`from`) and returned are UTF-16 code units, like
every string position (#377 phase 2b): a `from` between the halves of a pair starts after the
pair, and the offsets of a match are translated once per match.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_regex_new` | `(const VeltStr* pattern, const VeltStr* flags, IoResult<VeltRegex>* out)` | flags from `dgimsuvy`, each once (`g y d u v` don't change matching); bad pattern/flags ⇒ `EINVAL` with a JS-style message (`Invalid regular expression: /p/f: …`) |
| `velt_rt_regex_free` | `(VeltRegex re)` | |
| `velt_rt_regex_last_index` | `(VeltRegex re) -> f64` | JS `lastIndex`, kept with the handle (0 when compiled) |
| `velt_rt_regex_set_last_index` | `(VeltRegex re, f64 value)` | sets it (atomic: a handle may be shared between tasks) |
| `velt_rt_regex_group_count` | `(VeltRegex re) -> u64` | groups including group 0 |
| `velt_rt_regex_group_names` | `(VeltRegex re, VeltStrArray* out)` | names of groups 1.. (`""` = unnamed) |
| `velt_rt_regex_test` | `(VeltRegex re, const VeltStr* s, u64 from) -> u8` | a match at or after `from` |
| `velt_rt_regex_exec` | `(VeltRegex re, const VeltStr* s, u64 from, VeltArray<i64>* out) -> u8` | 1 ⇒ `out` = start/end per group (`-1` = did not take part); 0 ⇒ no match, `out` untouched |
| `velt_rt_regex_exec_all` | `(VeltRegex re, const VeltStr* s, VeltArray<i64>* out)` | all non-overlapping matches, `2 * group_count` offsets each |
| `velt_rt_regex_replace` | `(VeltRegex re, const VeltStr* s, const VeltStr* replacement, u8 all, VeltStr* out)` | JS `GetSubstitution` patterns `$$ $& $\` $' $n $nn $<name>` |
| `velt_rt_regex_split` | `(VeltRegex re, const VeltStr* s, u64 limit, VeltStrArray* out)` | JS `split` (captures spliced in, `""` for unmatched ones); `limit` 0 = none |
| `velt_rt_regex_escape` | `(const VeltStr* s, VeltStr* out)` | metacharacters backslash-escaped |

### 14.3 Child processes (`velt:child_process`)

```c
typedef struct {                 // std/child_process.vlt `CommandSpec` (104 bytes)
    VeltStr program;             // looked up in PATH when it has no separator
    VeltStrArray args;           // after the program name
    VeltStr cwd;                 // "" = the parent's
    VeltStrArray env;            // name, value, name, value, … added to the environment
    uint32_t stdio;              // stdin | stdout << 2 | stderr << 4; 0 inherit, 1 pipe, 2 ignore
    uint8_t clear_env;           // 1 = start from an empty environment
} VeltCommand;
typedef struct { int32_t code; uint32_t pad; VeltStr stdout; VeltStr stderr; } VeltOutput;  // 56 bytes
```
`VeltChild` is an `Arc`; in-flight operations hold a clone, so `close` is allowed any time. A
runtime task owns the OS child and reaps it (no zombies), so dropping a handle never kills.
Exit codes: the process's code, or `128 + signal` when killed by a signal (Unix). Spawn failures
are `IoResult` errors whose message starts with `spawn <program>: `.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_child_spawn` | `(const VeltCommand* spec, IoResult<VeltChild>* out)` | spec copied |
| `velt_rt_child_pid` | `(VeltChild c) -> u32` | |
| `velt_rt_child_exit_code` | `(VeltChild c) -> i64` | -1 while running |
| `velt_rt_child_wait` | `(VeltChild c) -> VeltFut*` | `IoResult<i32>` exit code |
| `velt_rt_child_kill` | `(VeltChild c, i32 signal, VeltErr* out)` | Unix signal number (Windows: terminate); no-op once finished |
| `velt_rt_child_close` | `(VeltChild c)` | releases the handle only |
| `velt_rt_child_read` | `(VeltChild c, u32 which, u64 max) -> VeltFut*` | `which` 1 stdout / 2 stderr; `IoResult<VeltBytes>`, empty = EOF, `max` 0 = 64 KiB; `EINVAL` if not piped |
| `velt_rt_child_read_string` | `(VeltChild c, u32 which, u64 max) -> VeltFut*` | `IoResult<VeltStr>`, split characters completed on the next read, `""` = EOF |
| `velt_rt_child_write` | `(VeltChild c, const VeltStr* data) -> VeltFut*` | `IoResult<()>` to stdin (copied) |
| `velt_rt_child_close_stdin` | `(VeltChild c) -> VeltFut*` | `IoResult<()>`; idempotent |
| `velt_rt_child_output` | `(const VeltCommand* spec, const VeltStr* input) -> VeltFut*` | run to completion: stdout/stderr piped and drained concurrently, `input` (empty = none, stdin is then null) written then closed; `IoResult<VeltOutput>`, output decoded lossily; `spec.stdio` ignored |
| `velt_rt_child_output_sync` | `(const VeltCommand* spec, const VeltStr* input, IoResult<VeltOutput>* out)` | same, blocking |

### 14.4 Standard input (`velt:stdin`)

`typedef struct { IoResult<VeltStr> line; u8 eof; } LineRead;` (64 bytes): a line without its
`\n`/`\r\n`; at end of input `eof = 1` and the line is empty. One process-wide buffered stream
serves both forms; async reads run on the blocking pool; text is decoded lossily.

| Symbol | Signature |
|---|---|
| `velt_rt_stdin_read_line` | `() -> VeltFut*` (result `LineRead`) |
| `velt_rt_stdin_read_line_sync` | `(LineRead* out)` |
| `velt_rt_stdin_read_all` | `() -> VeltFut*` (result `IoResult<VeltStr>`) |
| `velt_rt_stdin_read_all_sync` | `(IoResult<VeltStr>* out)` |

### 14.5 Machine facts (`velt:os`)

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_os_platform` | `(VeltStr* out)` | static `darwin` / `linux` / `win32` / Rust OS name |
| `velt_rt_os_arch` | `(VeltStr* out)` | static `arm64` / `x64` / Rust arch name |
| `velt_rt_os_cpu_count` | `() -> u64` | available parallelism, ≥ 1 |
| `velt_rt_os_tmpdir` | `(VeltStr* out)` | no trailing separator |
| `velt_rt_os_hostname` | `(VeltStr* out)` | `""` if unknown |

### 14.6 UDP and DNS (`velt:udp`, `velt:dns`)

`VeltUdp` is an `Arc<tokio::net::UdpSocket>` (in-flight operations hold a clone).
`typedef struct { VeltBytes data; VeltStr addr; } VeltDatagram;` (48 bytes; `addr` = `"ip:port"`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_udp_bind` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<VeltUdp>`; port 0 = any free port |
| `velt_rt_udp_port` | `(VeltUdp u) -> u32` | |
| `velt_rt_udp_send_to` | `(VeltUdp u, const VeltBytes* data, const VeltStr* addr) -> VeltFut*` | `IoResult<u64>` bytes sent; data and address copied, host names resolved |
| `velt_rt_udp_recv_from` | `(VeltUdp u, u64 max) -> VeltFut*` | `IoResult<VeltDatagram>`, payload truncated to `max` (0 = 64 KiB) |
| `velt_rt_udp_set_broadcast` | `(VeltUdp u, u8 on, VeltErr* out)` | |
| `velt_rt_udp_close` | `(VeltUdp u)` | |
| `velt_rt_dns_lookup` | `(const VeltStr* host) -> VeltFut*` | `IoResult<VeltStrArray>` of IP strings (system resolver, duplicates removed); `ENOENT` if unresolvable |

### 14.7 File streams (`velt:fs_stream`)

`VeltFileReader` / `VeltFileWriter` are `Arc`s around a buffered (64 KiB) `std::fs::File`;
every operation runs on the blocking pool. A writer's last release flushes (errors ignored).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_fs_open_read` | `(const VeltStr* path) -> VeltFut*` | `IoResult<VeltFileReader>` |
| `velt_rt_fs_reader_read` | `(VeltFileReader r, u64 max) -> VeltFut*` | `IoResult<VeltBytes>`, empty = EOF, `max` 0 = 64 KiB |
| `velt_rt_fs_reader_read_string` | `(VeltFileReader r, u64 max) -> VeltFut*` | `IoResult<VeltStr>`, split characters completed on the next call, `""` = EOF |
| `velt_rt_fs_reader_read_line` | `(VeltFileReader r) -> VeltFut*` | result `LineRead` (§14.4) |
| `velt_rt_fs_reader_close` | `(VeltFileReader r)` | |
| `velt_rt_fs_open_write` | `(const VeltStr* path, u8 append) -> VeltFut*` | `IoResult<VeltFileWriter>`; creates; truncates unless `append` |
| `velt_rt_fs_writer_write` / `_write_bytes` | `(VeltFileWriter w, const VeltBytes* data) -> VeltFut*` | `IoResult<()>`; data copied (two names so Velt can bind a string and a byte form) |
| `velt_rt_fs_writer_flush` | `(VeltFileWriter w) -> VeltFut*` | `IoResult<()>` |
| `velt_rt_fs_writer_close` | `(VeltFileWriter w) -> VeltFut*` | `IoResult<()>`: flush + close; later writes fail `EPIPE`; idempotent |
| `velt_rt_fs_writer_free` | `(VeltFileWriter w)` | releases the handle |

### 14.8 HTTPS, HTTP/2 and response headers (`velt:http` additions)

TLS is rustls with the `ring` provider (`crates/velt_rt/src/tls.rs`). Clients trust Mozilla's
root certificates (`webpki-roots`, compiled in) plus optional extra PEM CAs; servers offer
ALPN `h2` and `http/1.1`. The client (`velt_rt_http_fetch_send`, §7) accepts `https://` and
negotiates HTTP/2 when the server offers it.
HTTP/1.1 server connections now support upgrades (`serve_connection_with_upgrades`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_serve_tls` | `(const VeltStr* addr, const VeltHandler* h, const VeltStr* cert_pem, const VeltStr* key_pem) -> VeltFut*` | as `velt_rt_http_serve`, over TLS; certificate chain + PKCS#8/PKCS#1/SEC1 key; bad PEM or mismatch ⇒ `EINVAL`; handshakes time out after 10 s |
| `velt_rt_http_req_upgrade` | `(VeltReq r) -> u64` | key of the request's parked HTTP upgrade (0 = the request has no `Upgrade` header); an unclaimed upgrade is dropped when the handler's response is produced |
| `velt_rt_http_req_take_upgrade` | `(VeltReq r) -> u64` | claims the request's parked upgrade (once) and returns its connection as a `VeltStream` (§6), 0 if there is none or it was claimed. The handler must return a `101` response; the stream's first operation waits until it is sent (what is written before goes out after it) and fails with `ECONNABORTED` if the upgrade does not happen. Over plain TCP the stream is the raw socket |

### 14.9 WebSockets (`velt:websocket`)

`velt:websocket` is written in Velt (`std/websocket.vlt`, `std/websocket/`) on std/net streams
(§6): a client connects with `velt_rt_tcp_connect` / `velt_rt_tcp_connect_tls`, a server takes
the upgraded connection with `velt_rt_http_req_take_upgrade` (§14.8). A `WebSocket` is a struct
of the `TcpStream` and a `shared` value holding the connection's send queue and reader (one
lock), so there is no `VeltWs` handle and no `velt_rt_ws_*` symbol. It uses public APIs only
(reads with `TcpStream.read`, masking keys from `randomBytes`, text through `u8[]`'s `write` and
`utf8Slice`), except the process-wide slot of the topics (§2.2). A send encodes its frame into the connection's queue at the call (call order is wire
order, also before a server-side upgrade completes) and completes once it is queued; a flusher
task started by whoever queues into an idle connection writes everything queued in one write
(a reply by the connection's reading task, with no more input waiting, is written at once
instead), and a send waits while more than 1 MiB is queued. No callbacks are stored (§13.5).

### 14.10 HTML escaping (`velt:html`; stream std-2, additive)

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_html_escape` | `(const VeltStr* s, VeltStr* out)` | owned `s` with `& < > " '` → `&amp; &lt; &gt; &quot; &#39;`, in one pass (output sized once); other bytes unchanged. Without any of them, `s` itself (as `velt_rt_str_own`: shared, no copy) |
| `velt_rt_jsx_escape` | `(const VeltStr* s, VeltStr* out)` | as `velt_rt_html_escape`, but `'` → `&#x27;` (react-dom's; `std/jsx`) |

Stable hash (`velt:hash`, additive):

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_fnv1a64_str` | `(const VeltStr* s) -> u64` | `velt:hash`: 64-bit FNV-1a of the bytes; stable forever (std contract) |
| `velt_rt_fnv1a64_bytes` | `(const VeltBytes* data) -> u64` | the same over a `u8[]` |

### 14.11 SQLite (`velt:sqlite`; stream db, additive)

The crate `velt_std_sqlite` over `rusqlite` (SQLite compiled in: `bundled`; also
`column_decltype`), built on the native-package SDK only and linked into the runtime
(native_abi.md "Drivers in the runtime"): its functions are `velt_std_sqlite__*` exports with
the signatures below. **Synchronous**: a typical statement takes microseconds, less than a
blocking-pool round trip. A database is a registry key of `Mutex<Option<Connection>>` plus a
failure counter and the last error; a statement is a key of `{ Arc<db>, sql }` that re-finds
its compiled statement in the connection's cache (`prepare_cached`, 256 entries) on every call,
so closing the database while statements exist is safe (they fail with `SQLITE_MISUSE`). A
null or closed handle (a closed `Database` / `Statement` in std) fails the same way instead of
crashing.

Errors: `err.code` is SQLite's **extended result code** (never 0; e.g. 2067
`SQLITE_CONSTRAINT_UNIQUE`), `message` SQLite's message. Parameter problems are `SQLITE_RANGE`
(25), a closed handle or several statements in one `prepare` `SQLITE_MISUSE` (21).

Parameters (`params`) are JSON text (`velt_text::db_json`): `""` = none, an object binds
`:name` / `@name` / `$name` by name (missing member ⇒ `SQLITE_RANGE`, extra members ignored), an
array or a lone scalar binds by position (count must match). Integer literals in i64 range ⇒
INTEGER (exact), other numbers ⇒ REAL, strings ⇒ TEXT, `true`/`false` ⇒ 1/0, `null` ⇒ NULL,
arrays of integers 0–255 (`u8[]`) ⇒ BLOB, nested objects ⇒ `SQLITE_RANGE`. For the parameter
types it can, std hands the parameters to a sink instead (`bind_begin`, native_abi.md "Parameter
sinks"), which binds them as their JSON text would. Rows come through a row cursor
(native_abi.md "Row cursors"), which std decodes with `__intrinsic_json_parse_cursor<T>`: they
read as the JSON `[{"col":value,...}]` keyed by column name, INTEGER exact, REAL a number
(non-finite ⇒ `null`), TEXT a string, BLOB a number array, NULL `null`; a 0/1 value in a column
declared `BOOL`/`BOOLEAN` is `false`/`true`. The cursor holds the connection's lock until std
gives it back with `close_rows`, as a sink does until it is handed on or given up.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_std_sqlite__open` | `(string path, bool readonly, bool create, u32 timeout_ms) -> IoResult<u64>` | `":memory:"`, a file path or a `file:` URI; `timeout_ms` = busy timeout |
| `velt_std_sqlite__close` | `(u64 db) -> IoStatus` | closes the connection (finalizing cached statements) and releases the handle; null or closed ⇒ no-op |
| `velt_std_sqlite__exec` | `(u64 db, string sql) -> IoStatus` | `;`-separated statements, no parameters |
| `velt_std_sqlite__pragma` | `(u64 db, string source) -> IoResult<string>` | `PRAGMA <source>`: first column of the first row as text, `""` if none/NULL/blob |
| `velt_std_sqlite__in_transaction` | `(u64 db) -> bool` | not in autocommit mode; false when closed |
| `velt_std_sqlite__error_count` | `(u64 db) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_std_sqlite__reset_error_count` | `(u64 db, u64 count)` | a nested `transaction` that rolled back hands its failure to its caller |
| `velt_std_sqlite__last_error` | `(u64 db) -> IoStatus` | the most recent failure as a failed status (success if none) |
| `velt_std_sqlite__prepare` | `(u64 db, string sql) -> IoResult<u64>` | compiles exactly one statement; the connection keeps it per SQL text (preparing the same text again compiles nothing) |
| `velt_std_sqlite__run` | `(u64 stmt, string params) -> IoResult<{changes:i64,lastInsertRowid:i64}>` | steps to completion (rows discarded) |
| `velt_std_sqlite__open_rows` | `(u64 stmt, string params, bool first_only) -> IoResult<u64>` | binds, steps to the first row: a row cursor (`RawCursor*`) for `__intrinsic_json_parse_cursor<T[]>`; `first_only` ⇒ the first row alone (`<T>`), or 0 if there is none |
| `velt_std_sqlite__close_rows` | `(u64 cursor) -> IoStatus` | releases a cursor (and the connection's lock); fails with what SQLite met while stepping; 0 ⇒ no-op |
| `velt_std_sqlite__bind_begin` | `(u64 stmt) -> IoResult<u64>` | a parameter sink (`RawSink*`) for `__intrinsic_db_bind<P>` |
| `velt_std_sqlite__open_rows_bound` | `(u64 sink, bool first_only) -> IoResult<u64>` | `open_rows` with the parameters the sink bound (missing or extra ones fail as with JSON text) |
| `velt_std_sqlite__run_bound` | `(u64 sink) -> IoResult<{changes:i64,lastInsertRowid:i64}>` | `run` with the parameters the sink bound |
| `velt_std_sqlite__bind_abort` | `(u64 sink)` | gives up a sink std did not use (it then binds JSON text) |
| `velt_std_sqlite__note_error` | `(u64 stmt, i32 code, string message)` | counts a failure std found (a row that does not decode) against the connection |
| `velt_std_sqlite__stmt_free` | `(u64 stmt)` | releases the handle; null ⇒ no-op |
| `velt_std_sqlite__error_name` | `(i32 code) -> string` | known extended names (`SQLITE_CONSTRAINT_UNIQUE`, `SQLITE_BUSY_SNAPSHOT` …), else the primary one |

Not available on WebAssembly (no symbols in `libvelt_rt_wasm.a`: linking fails with undefined
`velt_std_sqlite__*`).

### 14.12 Redis (std/redis; stream db, additive)

The crate `velt_std_redis`, built on the native-package SDK only and linked into the runtime
(native_abi.md "Drivers in the runtime"): a RESP2 client over the runtime's own sockets
(`velt_native::net::TcpStream`, on Velt's scheduler the worker's reactor) and timers, with TLS
from the shared client configuration (`velt_tls`: `ring`, webpki roots + extra PEM CAs). A
client is a registry key of one **multiplexed** connection: requests from any number of tasks
are queued in order and written in batches, and a reader task completes them in order;
in-flight operations keep a reference, so `close` is allowed any time, and a closed handle
fails with `EBADF`. A failed connection fails the commands already written to it
(`ECONNRESET`); later commands reopen it (exponential backoff for up to 10 s, stream
linux-servers) or fail with the connect error. A subscriber is a key of a dedicated subscribed
connection whose messages are pulled (no callbacks are stored, §13.5).

URLs: `redis://[[user]:password@]host[:port][/db]` or `rediss://…` (TLS); percent-decoded
credentials, `[v6]` hosts, `?db=N`. The handshake sends `AUTH` and `SELECT` as needed; connecting
(TCP + TLS + handshake) times out after 10 s (`ETIMEDOUT`). A bad URL is `EINVAL`.

Errors: I/O failures use the §3 codes. An **error reply** from the server uses code **100**
with the reply text as the message (`WRONGTYPE Operation against …`); std/redis takes its first
word as `RedisError.code`. A reply that is not valid RESP is `EILSEQ`.

Commands are encoded from the borrowed `string[]` when called (an `#[export(async)]` `fn`
returning its future, native_abi.md); replies are written straight into Velt's arrays.

```
RawReply   { tags: u8[]; nums: i64[]; strs: string[] }
  One entry per node in pre-order (a node, then its children). tags: 0 nil, 1 status,
  2 error, 3 int, 4 bulk string, 5 array; nums: the int value or the array's length (else 0);
  strs: the text of status/error/string nodes (else ""; invalid UTF-8 becomes U+FFFD).
RawMessage { kind: u32; channel: string; message: string; pattern: string }
  kind: 0 = closed (after close), 1 = message, 2 = pattern message (pattern set)
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_std_redis__connect` | `async (string url, string ca) -> IoResult<u64>` | `ca`: extra PEM CAs for `rediss://` (`""` = none) |
| `velt_std_redis__duplicate` | `async (u64 c) -> IoResult<u64>` | a second connection opened like `c` |
| `velt_std_redis__close` | `(u64 c)` | releases the handle (through any copy; again: no-op) |
| `velt_std_redis__command` | `async (u64 c, string[] args) -> IoResult<RawReply>` | one command (`args[0]` its name); an error reply fails with code 100 |
| `velt_std_redis__pipeline` | `async (u64 c, string[] args, u64[] counts, bool atomic) -> IoResult<RawReply>` | `args` split by `counts`, sent together; one array node per command (error replies as nodes); `atomic`: in `MULTI`/`EXEC`, the result is `EXEC`'s array |
| `velt_std_redis__subscribe` | `async (u64 c, string url, string ca, string[] names, bool patterns) -> IoResult<u64>` | a new subscribed connection (to client `c`'s server, or `url` when `c` is 0); resolves once every name is confirmed |
| `velt_std_redis__sub_next` | `async (u64 s) -> IoResult<RawMessage>` | the next message; kind 0 after `close` |
| `velt_std_redis__sub_change` | `async (u64 s, string[] names, bool patterns, bool subscribe) -> IoStatus` | (un)subscribe, once confirmed |
| `velt_std_redis__sub_close` | `(u64 s)` | pending and later `next` calls return kind 0 |

Not available on wasm (no sockets): the symbols are not defined by `velt_rt_wasm`.

### 14.13 PostgreSQL (`velt:postgres`; stream db, additive)

The crate `velt_std_postgres`, built on the native-package SDK only (`velt_native`, `velt_text`) and linked into the runtime (native_abi.md "Drivers in the runtime"); its functions are `velt_std_postgres__*` exports, with the signatures below. Over `tokio-postgres` (`default-features = false`), connected over the runtime's own sockets (`velt_native::net::TcpStream`) and wire stream (§14.18). Rows are written into the result string itself (`velt_native::StrBuf`); `sql` and `params` are kept by reference, not copied.
TLS is a hand-written `TlsConnect` over tokio-rustls with the runtime's `ring` provider and
roots (§14.8); `sslmode` = `disable`, `prefer` (default), `require` (no certificate check),
`verify-ca`, `verify-full`, plus `sslrootcert=<PEM file>` (extra roots; turns `require` into
`verify-ca`). No channel binding (SCRAM falls back to SCRAM-SHA-256). Every server round trip is
an async leaf (§1). `VeltPgClient` is an `Arc` of `{ connection (tokio-postgres client + LRU
statement cache of 256 + transaction depth), pool lease, failure counter, last 32 failures }`;
operations keep an `Arc` of the connection, so `close` is safe while they run. `VeltPgPool` is
an `Arc` of `{ config, semaphore(max), idle connections }`. The null handle (a closed `Client`,
an ended `Pool` in std) fails with `ECLOSED` instead of crashing. No code pointers are stored
(§13.5): `transaction(fn)` is Velt over `begin` / `end` and the failure counter.

Errors: `err.code` is only the failure flag (the §3 code when an I/O error caused it, else 99);
`message` is JSON `{"code":…,"message":…,"detail":…|null,"constraint":…|null}` where `code` is
the SQLSTATE (`"23505"`) or a name: §3 names (`"ECONNREFUSED"`, `"ETIMEDOUT"` …),
`"ECONNRESET"` (connection lost), `"ECLOSED"`, `"EINVAL"` (connection string, parameters, bad
transaction nesting), `"ETLS"`, `"ENOTSUP"` (undecodable column type), `"UNKNOWN"`.

Parameters (`params`) are JSON text as in §14.11 (`db_json`): `""` = none, an array (or scalar)
binds `$1..$n` (count must match), an object binds `:name` / `$name` placeholders, which are
rewritten to `$n` once per SQL text and cached (`::` casts, literals, quoted identifiers, dollar
quotes and comments are skipped; mixing with `$1` ⇒ `EINVAL`; missing member ⇒ `EINVAL`). Values
are converted for the prepared parameter types: Int ⇒ int2/int4/int8/oid (range-checked),
float4/float8, numeric, text types; Float ⇒ float4/float8, numeric, text types; Bool ⇒ bool,
text types; Bytes ⇒ bytea; Text ⇒ text types and json/jsonb in binary, any other type in text
format (parsed by the server); Null ⇒ NULL; anything else ⇒ `EINVAL`. For the parameter
types it can, std hands them to a sink instead (`bind_begin`, native_abi.md "Parameter sinks"),
which keeps them as the JSON text would have parsed.
Rows come through a row cursor (native_abi.md "Row cursors", decoded by
`__intrinsic_json_parse_cursor`), the values as JSON text would give them (batches still return
JSON text, `db_json::RowWriter`): int2/int4/int8/oid exact, float4/float8 (non-finite
⇒ `null`), numeric ⇒ string of its digits, bool, text types/enums ⇒ string, uuid ⇒ string,
date/time/timestamp ⇒ ISO 8601 string, timestamptz ⇒ ISO 8601 UTC with `Z`, json/jsonb ⇒ the
document, bytea ⇒ number array, arrays ⇒ nested arrays, domains ⇒ base type, NULL ⇒ `null`;
other column types ⇒ `ENOTSUP` (checked before any row).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_std_postgres__connect` | `(const VeltStr* url) -> VeltFut*` | `IoResult<VeltPgClient>`; URL or libpq `key=value` string |
| `velt_std_postgres__query_rows` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params, u8 first_only) -> VeltFut*` | `IoResult<u64>`: a row cursor (`RawCursor*`) over the rows, given back to `close_rows`; `first_only` ⇒ the first row alone, or 0 if there is none |
| `velt_std_postgres__query_bound` | `(VeltPgClient c, const VeltStr* sql, u64 sink, u8 first_only) -> VeltFut*` | `query_rows` with the parameters a sink took |
| `velt_std_postgres__execute_bound` | `(VeltPgClient c, const VeltStr* sql, u64 sink) -> VeltFut*` | `execute` with the parameters a sink took |
| `velt_std_postgres__bind_begin` | `() -> u64` | a parameter sink (`RawSink*`) for `__intrinsic_db_bind<P>`, handed on to a `…_bound` call |
| `velt_std_postgres__bind_abort` | `(u64 sink)` | gives up a sink std did not use |
| `velt_std_postgres__close_rows` | `(u64 cursor, VeltErr* out)` | releases a row cursor; fails if a value could not be read; 0 ⇒ no-op |
| `velt_std_postgres__execute` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | `IoResult<i64>`: rows affected/returned |
| `velt_std_postgres__batch` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `VeltErr`: `;`-separated statements, simple protocol, not cached |
| `velt_std_postgres__begin` | `(VeltPgClient c) -> VeltFut*` | `IoResult<u32>`: new level (1 = `BEGIN`, n > 1 = `SAVEPOINT velt_tx_n`) |
| `velt_std_postgres__end` | `(VeltPgClient c, u32 depth, u8 commit) -> VeltFut*` | `VeltErr`: commit / roll back level `depth`, which must be the innermost (`EINVAL` otherwise); the level ends whatever the outcome |
| `velt_std_postgres__depth` | `(VeltPgClient c) -> u32` | open levels; 0 when closed |
| `velt_std_postgres__error_count` | `(VeltPgClient c) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_std_postgres__reset_error_count` | `(VeltPgClient c, u64 count)` | forget failures after `count` |
| `velt_std_postgres__error_after` | `(VeltPgClient c, u64 count, VeltErr* out)` | the first failure after `count` failures (code 0 if none) |
| `velt_std_postgres__note_error` | `(VeltPgClient c, const VeltStr* code, const VeltStr* message)` | counts a failure std found (`EMISMATCH` row decoding) |
| `velt_std_postgres__close` | `(VeltPgClient c)` | closes (or returns a pooled connection to its pool unless broken or in a transaction) and releases the handle; null ⇒ no-op |
| `velt_std_postgres__pool_new` | `(const VeltStr* url, u32 max, IoResult<VeltPgPool>* out)` | parses the URL and reads `sslrootcert` now; connections open on demand; `max == 0` ⇒ 10 |
| `velt_std_postgres__pool_connect` | `(VeltPgPool p) -> VeltFut*` | `IoResult<VeltPgClient>`: a dedicated connection (holds a permit) until `velt_std_postgres__close` |
| `velt_std_postgres__pool_query_rows` / `_pool_query_bound` / `_pool_execute_bound` | as the client's, with `VeltPgPool p` | on a borrowed connection |
| `velt_std_postgres__pool_execute` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | as `velt_std_postgres__execute` |
| `velt_std_postgres__pool_batch` | `(VeltPgPool p, const VeltStr* sql) -> VeltFut*` | as `velt_std_postgres__batch` |
| `velt_std_postgres__pool_idle` | `(VeltPgPool p) -> u32` | idle connections |
| `velt_std_postgres__pool_end` | `(VeltPgPool p)` | closes idle connections, fails later acquires with `ECLOSED`, releases the handle; null ⇒ no-op |

Linking: tokio-postgres' `whoami` needs `-framework SystemConfiguration -framework
CoreFoundation` on macOS and `secur32.lib` on Windows (added to `velt_link`'s native library
lists). Not available on WebAssembly (no `velt_std_postgres__*` symbols in `libvelt_rt_wasm.a`).

### 14.14 Releasing an HTTP handler (stream linux-servers, additive)
`VeltHandler.env` is null or a closure environment box whose first word is its drop function
(`void drop(void* env)`, may be null), as closure lowering lays it out. Once a server is closed
and its last connection and request have finished, the runtime calls that function once, so
the handler's captures are dropped (their `dispose()` hooks run). A `serve` that fails (the
address does not bind, the TLS certificate or key does not parse) calls it too, since no server
will. Before this, `env` was never freed. Every in-flight request keeps the server's handler state alive, so no request can see a
released environment; under `velt dev`, environments of replaced handlers (§13.5) are still
never freed.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_server_shutdown` | `(VeltServer s) -> VeltFut*` | like `velt_rt_http_server_close` (frees `s`), then result `()` once every in-flight request finished and `env` was released (`await server.shutdown()`) |

### 14.15 PostgreSQL `COPY` (stream linux-servers, additive)
Writers (`VeltPgCopyIn`) and readers (`VeltPgCopyOut`) are registry keys (`u64`, §3.2 tables).
Errors are the JSON `PgError` of §14.13 (`ECLOSED` for a released handle); failures count
against the client.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_std_postgres__copy_from` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyIn>`: `COPY … FROM STDIN` started |
| `velt_std_postgres__copy_write` / `_write_bytes` | `(VeltPgCopyIn w, const VeltStr*/VeltBytes* data) -> VeltFut*` | `IoResult<()>`; copied, batched into ~4 KiB messages |
| `velt_std_postgres__copy_end` | `(VeltPgCopyIn w) -> VeltFut*` | `IoResult<i64>` rows copied; releases `w` |
| `velt_std_postgres__copy_abort` | `(VeltPgCopyIn w)` | releases `w` unfinished: the server aborts the copy |
| `velt_std_postgres__copy_to` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyOut>`: `COPY … TO STDOUT` started |
| `velt_std_postgres__copy_read` / `_read_bytes` | `(VeltPgCopyOut r) -> VeltFut*` | `IoResult<VeltStr / VeltBytes>`: every chunk received so far (≥ 1, up to ~64 KiB), empty at the end |
| `velt_std_postgres__copy_close` | `(VeltPgCopyOut r)` | releases `r` (unread data is discarded) |

### 14.16 Fast random numbers (`velt:random`; stream linux-servers, additive)
| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_prng_f64` | `() -> f64` | uniform `[0, 1)`, 53 bits; per-thread wyrand seeded from §14.1 |
| `velt_rt_prng_range` | `(i64 min, i64 max) -> i64` | uniform `[min, max)` (unbiased); `min` if `max <= min` |


### 14.17 Streamed response bodies (a `Response` whose body is a `BodyStream`)
A `VeltResp` body is either complete (`velt_rt_http_resp_build` kinds 1 and 2, §7: sent with
an exact `Content-Length`, no extra allocation) or **streamed**: `velt_rt_http_resp_stream_open`
replaces the body with the receiving end of a bounded channel (8 chunks) and returns a writer,
`VeltRespWriter`, a registry key (§3.2, `u64`). A streamed body has no known length, so hyper
sends it with chunked transfer encoding (HTTP/1.1) or DATA frames (HTTP/2) and never computes a
`Content-Length`; the status and headers go out when the handler returns the response, before
the first chunk. std/http/stream.vlt fills the writer from the `BodyStream`, in a task that
keeps running after the handler returned (a stored promise of the handler's task, §1.1): it
writes and flushes each chunk, stops reading the stream once a write or flush returns 0, and
closes the writer (aborts it when reading the stream failed). No code pointers are stored
(§13.5).

Writes append a copy to the writer's buffer (`BytesMut`) and never wait; a flush hands the
buffer to the body as one chunk, waiting for channel room (**backpressure**: a producer faster
than its client waits in `flush`). A buffer that reaches 16 KiB is also handed over by a write
when the channel has room. Chunks keep their write order even when several tasks flush
concurrently. Once the client has gone away (hyper dropped the body) or the writer ended, writes
and flushes return 0 and discard their data. A writer that is neither closed nor aborted keeps
its response open.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_resp_stream_open` | `(VeltResp r) -> VeltRespWriter` | body becomes a stream (no default `content-type`); opening again detaches the earlier writer (its writes return 0). A bodiless status (1xx, 204, 304) keeps the empty body; the writer's writes return 0 |
| `velt_rt_http_resp_stream_write_bytes` | `(VeltRespWriter w, const VeltBytes* data) -> u8` | buffers a copy; 0 once ended, client gone, or `w` released |
| `velt_rt_http_resp_stream_flush` | `(VeltRespWriter w) -> VeltFut*` | result `u8`: 1 = the buffer was handed to the body (nothing buffered: 1 while the client is there); 0 = client gone / ended. Cancel-safe: the buffer is taken only once there is room |
| `velt_rt_http_resp_stream_close` | `(VeltRespWriter w) -> VeltFut*` | result `u8` as `flush`; sends the rest, ends the body normally (final chunk) and releases `w`; 0 on a released handle |
| `velt_rt_http_resp_stream_abort` | `(VeltRespWriter w)` | ends the body with an error (HTTP/1.1: the connection is closed without the final chunk; HTTP/2: `RST_STREAM`), discarding the buffer, and releases `w`; no-op on a released handle |

### 14.18 PostgreSQL batches (`std/postgres`; stream platform-perf, additive)
One prepared statement run with N parameter sets in one message group — `Bind` + `Execute` per
set and **one** `Sync` (pgx's `Batch`): one round trip and one implicit transaction on the server
instead of N. tokio-postgres has no API for it, so every connection now runs over a wire stream
(`postgres/wire`) between tokio-postgres and the socket/TLS stream that injects the group between
two driver requests and routes the server's replies up to its `ReadyForQuery` back to the batch.
The runtime opens the socket itself for that (`Config::connect_raw`): hosts/`hostaddr`s/ports in
order, `connect_timeout`, keepalives; `target_session_attrs` and `load_balance_hosts` are not
supported. The statement is prepared by tokio-postgres (types, cache) and a second time under a
batch name (`velt_b<n>`, `Parse` at the head of the first group; closed when evicted).

`sets` is JSON: an array with one element per execution, each a parameter set as §14.13
`params` (array ⇒ `$1..$n`, object ⇒ named placeholders, scalar ⇒ one parameter; the first
element decides named vs positional). `[]` ⇒ `"[]"` without a server round trip. Errors are the
§14.13 JSON `PgError`; when an execution fails, the server skips the rest of the group, so the
whole batch fails with that error (its SQLSTATE) and, outside a transaction, the group's
earlier writes are rolled back with it (all or nothing). Failures count against the client.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_std_postgres__query_batch` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | `IoResult<VeltStr>`: a JSON array, per execution: `mode` 0 its rows (`[[{…},…],…]`), 1 its first row or `null` (`[{…},null]`), 2 its rows affected (`[1,0]`) |
| `velt_std_postgres__pool_query_batch` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | as `velt_std_postgres__query_batch`, on one pooled connection |
