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
- `cx` is the Rust `&mut Context`; generated code only passes it on.
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
- Every runtime function returning `VeltFut*` allocates exactly that object and does no work until
  the first poll, except that a timer's deadline is fixed at creation (like Rust's `sleep`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_fut_poll` | `(VeltFut* f, void* cx) -> u32` | `f->poll(f, cx)` |
| `velt_rt_fut_drop` | `(VeltFut* f)` | cancel if pending, free; never drops the result slot |
| `velt_rt_fut_box` | `(PollFn, DropFn, const void* state, u64 size, u64 align) -> VeltFut*` | moves a compiled initial state into a heap future (for `Promise<T>` values that are stored, put in arrays, returned); result at +16 = state offset 0. `align ≤ 16`. Lazy until polled or started (§1.1). |
| `velt_rt_fut_start` | `(VeltFut* f, void (*result_drop)(void* slot))` | start a boxed promise now (§1.1); no-op for any other future, or outside a task. `result_drop` disposes of an unclaimed result (null if nothing to do); for a promise that can reject it reports an `Err` like an unhandled rejection. |
| `velt_rt_fut_transfer` | `(VeltFut* f, void (*transfer)(void* slot))` | the owner hands `f` to another task: `transfer` (compiled transfer glue, in place) runs on the result slot on the task that produces the result, as it finishes (at once if a started promise already finished); a race passes it to its children; no-op for join handles (a task transfers its own result: `velt_rt_spawn_transfer`, or the mark of the promise `velt_rt_spawn_fut` runs) and runtime leaves. WebAssembly does the same on its one thread, so values are moved or copied where native targets move or copy them. |
| `velt_rt_yield_now` | `(void* cx)` | `await yieldNow()` inline: call it, then `return 0`; resumes after other ready tasks. No allocation. |
| `velt_rt_yield_now_fut` | `() -> VeltFut*` | `yieldNow()` as a value; result: none |
| `velt_rt_sleep` | `(i64 ms) -> VeltFut*` | `sleep(ms)`; negative = 0; result: none |
| `velt_rt_all` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results) -> VeltFut*` | `Promise.all(array)`: takes ownership of the `n` futures (not of the pointer array); child `i`'s result is moved to `results + i*result_size` (must stay valid until completion/drop); concurrent, only woken children are re-polled. Result: none. |
| `velt_rt_all_with_drop` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | same as `velt_rt_all` (which = this with `result_drop == NULL`), for results that own resources: if the returned future is dropped **before completing**, `result_drop(results + i*result_size)` runs for every child `i` that had already finished (pending children are cancelled via their own drop). After completion it never runs � all results belong to the awaiter. Use it whenever `T` needs dropping. |
| `velt_rt_all_or_reject` | `(VeltFut* const* futs, u64 n, u64 result_size, void* results, void (*result_drop)(void* slot)) -> VeltFut*` | `Promise.all` over promises that can reject: like `velt_rt_all_with_drop` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled), but completes as soon as a child rejects. Its result is empty. On a rejection the rejected child's result is moved to slot 0 and is the only initialized slot (so a rejection is an `Err` tag in slot 0, `n > 0`): the runtime drops the other finished results with `result_drop` (null: nothing to drop) and drops the pending children (started promises keep running; mark them with `velt_rt_futs_handled` first). |

| `velt_rt_race` | `(VeltFut* const* futs, u64 n, u64 result_size) -> VeltFut*` | `Promise.race(array)`: takes ownership of the `n` futures (not of the pointer array); the first child to finish moves its `result_size`-byte result to the returned future's slot (+16) and the others are dropped (started promises keep running, §1.1). `n == 0` never completes. |
| `velt_rt_race_ok` | `(VeltFut* const* futs, u64 n, u64 result_size, void (*reject_drop)(void* slot)) -> VeltFut*` | `Promise.any`: like `velt_rt_race` over `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled): the first fulfilled child wins; a rejected one is dropped with `reject_drop` (null: nothing to drop) while others are still running, and the last rejection is the result when all reject. |
| `velt_rt_fut_detach` | `(VeltFut* f, void (*quiet_drop)(void* slot))` | the owner gives up `f` without cancelling it (pending siblings of an early `Promise.all` rejection). A boxed promise that is still lazy, even one its owner has polled, becomes a started promise of the current task without being polled now: it runs at the task's next poll, after the owner's continuation, as in JS, where the rejection handler runs before other woken promises. A started promise keeps running. Either way its result is disposed of with `quiet_drop` (handled, null: nothing to drop). Any other future is dropped as by `velt_rt_fut_drop`. Takes ownership of `f`. |
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
  drives them to completion and holds a keep-alive reference (the program entry waits, §7). A
  cancelled task (dropped before finishing, e.g. an HTTP request whose client disconnected)
  cancels its unfinished local promises (their `DropFn` runs); awaiting one of those afterwards
  is a fatal error.
- Outside a task (no `block_on` running, e.g. synchronous `main`), `velt_rt_fut_start` does
  nothing: the promise runs when it is awaited or spawned.
- Layout: a boxed future is `[head (96 bytes)][VeltFut hdr][state]`; the head holds the state's
  poll/drop, `result_drop`, the reference count, flags, the set and the awaiter's waker. Code
  pointers live only there and in the `VeltFut` header (hot-reload rule, §13.5).

## 2. Tasks

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_block_on` | `(PollFn poll, void* state)` | `async main`: `velt_main` builds the state on its stack, calls this, then reads the result at `state+0`. Runs the root as a task on the runtime's workers; returns when READY. Tasks still running afterwards are abandoned when the process exits. Must not be called from inside a task. |
| `velt_rt_spawn` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | `spawn(f(...))`: copies the initial state into the task (caller gives up ownership of its contents), starts it now. Returns the join handle; its result slot (+16) receives `result_size` bytes (≤ 256; box larger results). Dropping the handle **detaches** (task keeps running); a result the handle never claims (dropped before or after the task finished) is dropped with `result_drop` (null: nothing to drop). |
| `velt_rt_spawn_transfer` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align, u64 result_size, void (*result_drop)(void* slot), void (*result_transfer)(void* slot)) -> VeltFut*` | `velt_rt_spawn` for a result that can reach counted objects: `result_transfer` (compiled transfer glue, in place) runs on the result as the task's state finishes, on the task and inside its local set (so promises the task started, which may still use the result's objects, are on the same thread, and a promise the glue starts joins the set), before the join handle can see it. Null: as `velt_rt_spawn`. |
| `velt_rt_spawn_detached` | `(PollFn, DropFn, const void* state, u64 state_size, u64 state_align)` | spawn whose result is unused: no handle, one allocation |
| `velt_rt_spawn_fut` | `(VeltFut* f, u64 result_size, void (*result_drop)(void* slot)) -> VeltFut*` | `spawn(p)` where `p` is already a heap future (boxed promise, leaf); takes ownership of `f`; `result_drop` as for `velt_rt_spawn` |

Runtime: created lazily, workers = `VELT_THREADS` (positive integer) or the number of cores.
Allocation per task: states ≤ 1 KiB (align ≤ 16) live inline in tokio's task cell (size classes
64/256/1024), so a detached spawn is 1 allocation and a joinable one 2 (task + join handle).
Panics inside tasks print `panic: <msg>` and exit 101 (same hook as sync code).

### 2.1 Latches (`new Promise`)

A one-shot latch (`u64` handle, §3.2): the waking half of `new Promise` (std/prelude/promise.vlt
keeps the settled value in Velt code). Thread-safe; no callbacks are stored (§13.5).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_latch_new` | `() -> u64` | a closed latch |
| `velt_rt_latch_free` | `(u64 latch)` | releases the handle (a pending wait keeps the latch alive) |
| `velt_rt_latch_open` | `(u64 latch)` | opens it and wakes every waiter; a no-op when open |
| `velt_rt_latch_wait` | `(u64 latch) -> VeltFut*` | completes (unit result) once the latch is open |
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
sender still shares is deep-copied). No code pointers are stored, except the `item_drop` a pending
`send` future owns (§13.5).

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

### 2.3 Abort signals (`velt:task`)

Each signal's `u64` handle (an `Arc`) is owned by a private `shared` cell in std/task.vlt and
released once, at the cell's last reference; no handle is public. Aborting sets a flag, stores
the reason and wakes the waiters; it never cancels anything itself. No code pointers are
stored: `AbortSignal.timeout` is a runtime timer task holding a weak reference. A signal made
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
`VeltFileWriter`, `VeltWs`, `VeltJson`) are **`uint64_t` handles**, 0 = none. Velt has no
pointer type, so `std` stores them in `u64` fields and passes `u64` arguments; the runtime takes
and returns the same `u64`, in argument lists, results and result slots alike.
- Handles that `std` wraps in **Copy structs** (`VeltListener`, `VeltStream`, `VeltUdp`,
  `VeltChild`, `VeltFileReader`, `VeltFileWriter`, `VeltWs`) are keys into runtime handle tables
  (`velt_rt::registry::Key<T>`: generation, shard and slot), because Velt code may hold several copies:
  releasing one (`close`/`free`) makes every copy dead. Later operations fail with `EBADF`
  (code 16, "handle is closed"), sync accessors return their documented empty value (port 0,
  pid 0, exit code -1, empty address), and releasing again is a no-op; a stale handle never
  reaches a newer object. In-flight operations hold their own `Arc`, so the object lives until
  they finish.
- The HTTP handles (`VeltServer`, `VeltReq`, `VeltResp`, `VeltFetchResp`) are registry keys as
  well, so a stale or forged one never reaches memory (additive): server operations on a closed
  handle are no-ops (port 0, `shutdown` resolves at once), response builders ignore a dead
  response (`velt_rt_http_resp_header` returns 0) and the server answers 500 for one, and using a
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
| `velt_rt_str_array_drop` | `(VeltStrArray* a)` — drops elements + buffer, zeroes |

## 5. std/fs

Arguments are copied at the call (the caller keeps ownership). Paths are UTF-8 strings. Data
arguments are strings (`VeltStr`).

```c
typedef struct { uint64_t size; double mtime_ms; uint8_t is_file; uint8_t is_dir; } VeltStat; // 24 bytes
```

| Async (`-> VeltFut*`) | Sync (`..., out)`, returns nothing) | Result type |
|---|---|---|
| `velt_rt_fs_read_file(path)` | `velt_rt_fs_read_file_sync(path, out)` | `IoResult<VeltStr>` (`EILSEQ` if not UTF-8) |
| `velt_rt_fs_read_file_bytes(path)` | `velt_rt_fs_read_file_bytes_sync(path, out)` | `IoResult<VeltBytes>` |
| `velt_rt_fs_write_file(path, data)` | `velt_rt_fs_write_file_sync(path, data, out)` | `IoResult<()>` (= `VeltErr`) |
| `velt_rt_fs_append_file(path, data)` | `velt_rt_fs_append_file_sync(path, data, out)` | `IoResult<()>`; creates the file |
| `velt_rt_fs_read_dir(path)` | `velt_rt_fs_read_dir_sync(path, out)` | `IoResult<VeltStrArray>` names, sorted |
| `velt_rt_fs_stat(path)` | `velt_rt_fs_stat_sync(path, out)` | `IoResult<VeltStat>` (follows symlinks) |
| `velt_rt_fs_mkdir(path, u8 recursive)` | `velt_rt_fs_mkdir_sync(path, recursive, out)` | `IoResult<()>` |
| `velt_rt_fs_remove(path, u8 recursive)` | `velt_rt_fs_remove_sync(path, recursive, out)` | `IoResult<()>`; file, symlink, empty dir, or tree if recursive |
| `velt_rt_fs_rename(from, to)` | `velt_rt_fs_rename_sync(from, to, out)` | `IoResult<()>` |
| `velt_rt_fs_copy_file(from, to)` | `velt_rt_fs_copy_file_sync(from, to, out)` | `IoResult<()>` |
| `velt_rt_fs_exists(path)` | `velt_rt_fs_exists_sync(path) -> u8` | `u8` (never fails) |

All `path`/`from`/`to`/`data` parameters are `const VeltStr*`. Async variants run on tokio's
blocking pool. Error messages are Node's: `<CODE>: <description>, <syscall> '<path>'` (plus
` -> '<to>'` for `rename`/`copyfile`), e.g. `ENOENT: no such file or directory, lstat 'x'` from
`fs_remove`; the file streams' `open_read`/`open_write` (§14.7) use the same form. A failed
read or write of an opened file names no path (`EISDIR: illegal operation on a directory,
read`), and a directory opened as a file is `EISDIR` on every system (Windows reports access
denied).

## 6. std/net (TCP)

Handles (`VeltListener`, `VeltStream`: §3.2) are `Arc`s; in-flight operations keep the socket
alive, so `close` may be called at any time.

| Symbol | Signature | Result slot / notes |
|---|---|---|
| `velt_rt_tcp_listen` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<VeltListener>`; `addr` = `"host:port"`, port 0 = any free port |
| `velt_rt_tcp_listener_port` | `(VeltListener l) -> u32` | the real bound port |
| `velt_rt_tcp_listener_close` | `(VeltListener l)` | |
| `velt_rt_tcp_accept` | `(VeltListener l) -> VeltFut*` | `IoResult<VeltStream>` (TCP_NODELAY on) |
| `velt_rt_tcp_connect` | `(const VeltStr* addr) -> VeltFut*` | `IoResult<VeltStream>` |
| `velt_rt_tcp_read` | `(VeltStream s, u64 max) -> VeltFut*` | `IoResult<VeltBytes>` 1..max bytes, empty = EOF; `max == 0` ⇒ 64 KiB |
| `velt_rt_tcp_read_string` | `(VeltStream s, u64 max) -> VeltFut*` | `IoResult<VeltStr>` next chunk as UTF-8 (split characters are completed on the next call, invalid bytes → U+FFFD), `""` = EOF |
| `velt_rt_tcp_write` | `(VeltStream s, const VeltStr* data) -> VeltFut*` | `IoResult<()>` after all bytes are written; `data` is copied |
| `velt_rt_tcp_write_bytes` | `(VeltStream s, const VeltBytes* data) -> VeltFut*` | same for a `u8[]` |
| `velt_rt_tcp_shutdown` | `(VeltStream s, VeltErr* out)` | half-close (peer reads EOF) |
| `velt_rt_tcp_peer_addr` | `(VeltStream s, VeltStr* out)` | `"ip:port"` |
| `velt_rt_tcp_close` | `(VeltStream s)` | releases the handle |

## 7. std/http (hyper 1.x, HTTP/1.1 keep-alive + h2c)

**Handler.** A handler `(req: Request) => Promise<Response>` is described by:
```c
typedef struct {
    void     (*init)(void* env, void* req, void* state);    // write the initial state; owns req
                                                            // (the VeltReq handle as a pointer)
    uint32_t (*poll)(void* state, void* cx);                // result at state+0: VeltResp (0 ⇒ 500)
    void     (*drop)(void* state);                          // request cancelled (client gone)
    uint64_t state_size, state_align;
    void*    env;   // closure captures; shared read-only by concurrent requests (§14.14)
} VeltHandler;      // 48 bytes
```
The runtime stores each request's state inline in the request future (no extra allocation for
states ≤ 1 KiB) and polls it on a worker. The request body is fully read before `init` runs.
The handler must `velt_rt_http_req_drop(req)` when done with it (typically before returning).
`VeltReq` is a registry key (`crate::registry`, like the database handles): every accessor checks
it, and using a request after it was dropped stops the program with a clear message instead of
reading freed memory.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_serve` | `(const VeltStr* addr, const VeltHandler* h) -> VeltFut*` | result `IoResult<VeltServer>` once bound; serving continues in the background. `h` is copied. |
| `velt_rt_http_server_port` | `(VeltServer s) -> u32` | real port (after port 0) |
| `velt_rt_http_server_close` | `(VeltServer s)` | stop accepting, finish in-flight requests, close idle connections; frees `s` |
| `velt_rt_http_server_detach` | `(VeltServer s)` | free `s` but keep serving (dropping the `Server` value) |
| `velt_rt_http_server_shutdown` | `(VeltServer s) -> VeltFut*` | `close` + result `()` once the handler was released (§14.14) |
| `velt_rt_http_req_method` / `_path` / `_query` / `_body` | `(VeltReq r, VeltStr* out)` | owned copies; path excludes the query; query excludes `?`; body decoded lossily |
| `velt_rt_http_req_body_bytes` | `(VeltReq r, VeltBytes* out)` | |
| `velt_rt_http_req_header` | `(VeltReq r, const VeltStr* name, VeltStr* out) -> u8` | case-insensitive; 0 = absent (`out` untouched) |
| `velt_rt_http_req_header_count` | `(VeltReq r) -> u64` | |
| `velt_rt_http_req_header_at` | `(VeltReq r, u64 i, VeltStr* name, VeltStr* value)` | lowercase name |
| `velt_rt_http_req_header_names` | `(VeltReq r, VeltStrArray* out)` | every header name (lowercase), in received order: `req.headers` in one call (`header_at` per index is O(n) each) |
| `velt_rt_http_req_header_values` | `(VeltReq r, VeltStrArray* out)` | every value, in the order of `header_names` (non-UTF-8 bytes decoded lossily) |
| `velt_rt_http_req_drop` | `(VeltReq r)` | |
| `velt_rt_http_resp_new` | `(u32 status) -> VeltResp` | invalid status ⇒ 500 |
| `velt_rt_http_resp_header` | `(VeltResp r, const VeltStr* name, const VeltStr* value) -> u8` | append; 0 if invalid |
| `velt_rt_http_resp_body_text` | `(VeltResp r, VeltStr* body)` | **takes** `body` (zero-copy if owned; `*body` left empty); default `text/plain; charset=utf-8` |
| `velt_rt_http_resp_body_bytes` | `(VeltResp r, VeltBytes* body)` | takes; default `application/octet-stream` |
| `velt_rt_http_resp_json` | `(VeltResp r, VeltStr* json)` | takes; sets `application/json` |
| `velt_rt_http_resp_drop` | `(VeltResp r)` | only for responses not returned from a handler |

A listening server holds a **keep-alive** reference (released by `close`): after `main` returns,
the program entry waits until no keep-alive references remain — like Node, a listening server
keeps the process running. (`velt_rt_block_on` itself does not wait.)

`Response.text(b, s)` = `resp_new(s)` + `resp_body_text(r, &b)`; `Response.json(v, s)` = serialize
`v` (compiler-generated) + `resp_new(s)` + `resp_json`. For a bodiless status (1xx, 204, 304) the
body setters and `resp_json` drop the body and add no `content-type`.

**Client** (`http://` only; `https://` fails with `ENOTSUP`):

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_fetch` | `(const VeltStr* method, const VeltStr* url, const VeltStr* headers, u64 n_headers, const VeltStr* body) -> VeltFut*` | result `IoResult<VeltFetchResp>`; `headers` = `2*n` strings name,value,…; `body` may be null; all copied; body is read fully before READY |
| `velt_rt_http_fetch_resp_status` | `(VeltFetchResp r) -> u32` | |
| `velt_rt_http_fetch_resp_header` | `(VeltFetchResp r, const VeltStr* name, VeltStr* out) -> u8` | case-insensitive |
| `velt_rt_http_fetch_resp_text` | `(VeltFetchResp r, IoResult<VeltStr>* out)` | copy; `EILSEQ` if not UTF-8. (`await r.text()` can be a trivial compiled wrapper.) |
| `velt_rt_http_fetch_resp_bytes` | `(VeltFetchResp r, VeltBytes* out)` | copy |
| `velt_rt_http_fetch_resp_drop` | `(VeltFetchResp r)` | |

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
| `velt_rt_perf_now` | `() -> f64` | `performance.now()`: ms since process start, monotonic |
| `velt_rt_date_now` | `() -> i64` | `Date.now()`: ms since the Unix epoch |
| `velt_rt_exit` | see rt_abi.md | |
| `velt_rt_memory_rss` | `() -> i64` | `process.memoryUsage().rss`: resident set size in bytes from the OS (`/proc/self/statm`, `task_info`, `GetProcessMemoryInfo`); 0 if unknown. wasm: the linear memory size |
| `velt_rt_memory_heap` | `() -> i64` | `heapUsed`: mimalloc's committed heap bytes (Windows: the process's private committed bytes); without mimalloc, the RSS. wasm: the linear memory size |

## 9. Shared state and helpers (sync)

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_rc_inc` | `(u64* rc)` | `shared.clone()`; Relaxed (like `Arc`); aborts on overflow |
| `velt_rt_rc_dec` | `(u64* rc) -> u8` | Release; returns 1 (after an Acquire fence) when it reached zero: caller drops + frees |
| `velt_rt_atomic_add_i64` | `(i64* p, i64 delta) -> i64` | returns the **new** value; SeqCst |
| `velt_rt_atomic_load_i64` | `(i64* p) -> i64` | |
| `velt_rt_atomic_store_i64` | `(i64* p, i64 v)` | |
| `velt_rt_mutex_init` | `(u64* lock)` | the `Mutex<T>` lock word: 8 bytes, align 8, inside the shared object; no destroy needed |
| `velt_rt_mutex_lock` / `velt_rt_mutex_unlock` | `(u64* lock)` | `m.with(f)` = lock; call f; unlock. Blocks the thread; bodies must not await. |
| `velt_rt_str_hash` | `(const VeltStr* s) -> u64` | string Map/Set keys; fixed seed (deterministic), not flood-resistant |
| `velt_rt_math_sqrt` / `_floor` / `_ceil` / `_round` / `_trunc` / `_fabs` | `(f64) -> f64` | `round` = JS `Math.round` (ties toward +∞, keeps `-0`) |

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

**Invariant:** a count-1 buffer is appended to (and so possibly reallocated) only while no
borrowed static-form view into it is live. The only such views today are the JSON reader's
borrowed keys (§12.3), which point into the source text, and generated decode glue never
appends to the text it is reading. Appended text may lie in the target's own buffer (a share,
an uncounted copy or a static-form view of it): it is copied out before the buffer grows.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_strbuf_new` | `(u64 cap, VeltStrBuf* out)` | `cap` = initial capacity hint (≤ 23: starts inline, else allocates up front) |
| `velt_rt_strbuf_push_str` | `(VeltStrBuf* b, const VeltStr* s)` | `s` may be `b` itself or lie in its buffer |
| `velt_rt_strbuf_push_bytes` | `(VeltStrBuf* b, const u8* p, u64 len)` | static text chunks of a template; `len == 0` ⇒ `p` unused. The low 32 bits of `len` are the byte count; the high 32 bits may carry the UTF-16 unit count (as a string's `w1`): equal to the byte count, the text is ASCII and is not scanned; 0 means unknown (counted) |
| `velt_rt_strbuf_push_i64` / `_u64` | `(VeltStrBuf* b, i64 / u64 v)` | decimal |
| `velt_rt_strbuf_push_f64` | `(VeltStrBuf* b, f64 v)` | JS `String(v)` (same formatter as `velt_rt_write_f64`) |
| `velt_rt_strbuf_push_json_f64` | `(VeltStrBuf* b, f64 v)` | like `JSON.stringify`: JS format, `null` for NaN/±Infinity |
| `velt_rt_strbuf_push_inspect_str` | `(VeltStrBuf* b, const VeltStr* s)` | a string as `console.log` shows it inside a container (node `util.inspect` quoting and escaping) |
| `velt_rt_strbuf_push_inspect_key` | `(VeltStrBuf* b, const VeltStr* s)` | an object key as `console.log` shows it: bare if it matches `[A-Za-z_][A-Za-z0-9_]*` (node quotes `$`), else quoted like `push_inspect_str` |
| `velt_rt_strbuf_push_bool` | `(VeltStrBuf* b, u8 v)` | `true`/`false` |
| `velt_rt_strbuf_push_byte` | `(VeltStrBuf* b, u8 c)` | punctuation in generated glue |
| `velt_rt_strbuf_push_json_str` | `(VeltStrBuf* b, const VeltStr* s)` | quoted + escaped exactly like `JSON.stringify(s)`: `\"` `\\` `\b \f \n \r \t`, other controls < U+0020 as lowercase 6-char `\u00xx`; everything else verbatim |
| `velt_rt_strbuf_push_json_value` | `(VeltStrBuf* b, const void* v)` | `JSON.stringify(v)` of a `json.Value` field (null handle ⇒ `null`); emitted by the compiler, which passes the handle's address as a pointer (VIR `ptr`), unlike the `u64` handles of §3.2 |
| `velt_rt_strbuf_push_inspect_json` | `(VeltStrBuf* b, const void* v, u8 top)` | what `console.log` prints for a `json.Value` (node `util.inspect` of the parsed value: `{ a: 1, b: [ 2, 'x' ] }`, `[]`, `{}`, strings quoted like `push_inspect_str`, keys like `push_inspect_key`, one line at any depth); a string is raw when `top != 0`; null handle ⇒ `null`. The handle is passed like `push_json_value`'s (additive) |
| `velt_rt_strbuf_inspect_begin` | `()` | start of a top-level value printed by `console.log`, `${x}` or `String(x)`: the `<ref *N>` numbering of cycles starts over (node numbers per argument), unless an object is being printed (additive) |
| `velt_rt_strbuf_inspect_enter` | `(VeltStrBuf* b, const void* p) -> u8` | start printing the object at `p` (class instance or recursive object): `1`, or, when `p` is already being printed (a cycle), append `[Circular *N]` and return `0` (the caller skips it); `N` is the object's number for the whole top-level value (additive) |
| `velt_rt_strbuf_inspect_leave` | `(VeltStrBuf* b)` | done with the innermost entered object; an object with a number gets the `<ref *N> ` prefix at the start of its text (additive) |
| `velt_rt_strbuf_finish` | `(VeltStrBuf* b, VeltStr* out)` | moves the text to `*out`; `*b` becomes empty (reusable, nothing to free) |
| `velt_rt_strbuf_drop` | `(VeltStrBuf* b)` | abandon an unfinished builder (exception path); zeroes it |

### 12.2 String methods

**POC indexing model:** indexes and lengths are **byte offsets** (they agree with `s.length`). An
offset that falls inside a multi-byte character is moved back to that character's first byte
(`slice`) so results are always valid UTF-8. Where JS works per UTF-16 code unit (`split("")`,
`replaceAll("", x)`), these work per Unicode scalar value. For ASCII text everything matches JS
exactly. "Omitted" JS arguments are passed as the value given in Notes.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_str_slice` | `(const VeltStr* s, i64 start, i64 end, VeltStr* out)` | JS `slice`: negative = from end, clamped, empty if `start >= end`; omitted `end` = `i64::MAX` |
| `velt_rt_str_index_of` | `(const VeltStr* s, const VeltStr* needle, i64 from) -> i64` | `from` clamped to `0..=len` (omitted = 0); -1 if absent; empty needle ⇒ clamped `from` |
| `velt_rt_str_last_index_of` | `(const VeltStr* s, const VeltStr* needle, i64 from) -> i64` | last match starting `<= from`; omitted `from` = `i64::MAX` |
| `velt_rt_str_includes` | `(const VeltStr* s, const VeltStr* needle) -> u8` | (a `position` arg ⇒ use `index_of(s, n, pos) >= 0`) |
| `velt_rt_str_starts_with` / `_ends_with` | `(const VeltStr* s, const VeltStr* affix) -> u8` | |
| `velt_rt_str_eq` | `(const VeltStr* a, const VeltStr* b) -> u8` | `==` fast path: length check + one memcmp |
| `velt_rt_str_join` | `(const VeltStrArray* parts, const VeltStr* sep, VeltStr* out)` | `parts.join(sep)`: sums the pieces' lengths and unit counts, then writes the result once (inline, or a heap buffer of exactly its size) |
| `velt_rt_str_split` | `(const VeltStr* s, const VeltStr* sep, VeltStrArray* out)` | JS semantics: `"a,b,".split(",")` = `["a","b",""]`, `"".split(",")` = `[""]`, `"".split("")` = `[]`, `split("")` = characters. Drop with `velt_rt_str_array_drop` (§4). |
| `velt_rt_str_trim` / `_trim_start` / `_trim_end` | `(const VeltStr* s, VeltStr* out)` | JS WhiteSpace + LineTerminator set (includes U+FEFF, U+00A0, U+2028/9, Zs; not U+0085) |
| `velt_rt_str_to_upper` / `_to_lower` | `(const VeltStr* s, VeltStr* out)` | full Unicode default case mapping (`ß` → `SS`, final sigma), like JS; ASCII fast path |
| `velt_rt_str_replace` | `(const VeltStr* s, const VeltStr* from, const VeltStr* to, VeltStr* out)` | first occurrence; `to` expands JS patterns `$$`, `$&`, `` $` ``, `$'` (others literal) |
| `velt_rt_str_replace_all` | same | all non-overlapping occurrences; empty `from` inserts at every character boundary |
| `velt_rt_str_repeat` | `(const VeltStr* s, i64 n, VeltStr* out) -> u8` | 1 = ok; 0 = JS `RangeError` (n < 0 or result too large), `*out` empty — the compiler throws/panics |
| `velt_rt_str_pad_start` / `_pad_end` | `(const VeltStr* s, i64 target, const VeltStr* fill, VeltStr* out)` | lengths in bytes; the last partial `fill` is cut at a character boundary (non-ASCII fill may end up to 3 bytes short); omitted `fill` = `" "` |
| `velt_rt_str_char_code_at` | `(const VeltStr* s, i64 i) -> i64` | the **byte** at `i` (POC); -1 if out of range (JS: NaN) |
| `velt_rt_str_from_char_code` | `(i64 code, VeltStr* out)` | JS `ToUint16(code)`, UTF-8 encoded; lone surrogates → U+FFFD; ASCII results are static (no allocation) |
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
| `velt_rt_json_reader_new_with` | `(const VeltStr* src, u32 flags, u32 max_depth) -> VeltJsonReader*` | `JSON.parse` options: flag 1 = `skip_unknown` fails; `max_depth` = most arrays/objects open at once, counted from the top-level value (0 = no limit), checked by `expect_*_start`, `skip_value` and `read_value` |
| `velt_rt_json_reader_free` | `(VeltJsonReader* r)` | null ok |
| `velt_rt_json_reader_peek` | `(VeltJsonReader* r) -> u32` | next token kind, skipping whitespace: 0 EOF, 1 `null`, 2 `true`, 3 `false`, 4 number, 5 string, 6 `[`, 7 `]`, 8 `{`, 9 `}`, 10 error (bad byte, or the reader already failed). Classifies by first byte only; the `read_*` call validates. |
| `velt_rt_json_reader_expect_object_start` | `(r) -> u8` | consume `{` |
| `velt_rt_json_reader_next_key` | `(r, VeltStr* out) -> u8` | **1** = `*out` is the next key and its `:` is consumed (read the value next); **0** = `}` consumed (end of object); **2** = error. Handles the commas. The key **borrows** the source (static form) unless it had escapes — compare it, don't keep it (`velt_rt_str_own` makes a copy to keep: `velt_rt_str_clone` keeps the borrow); dropping it is always allowed. |
| `velt_rt_json_reader_expect_array_start` | `(r) -> u8` | consume `[` |
| `velt_rt_json_reader_array_next` | `(r) -> u8` | **1** = another element follows (read it next), **0** = `]` consumed, **2** = error |
| `velt_rt_json_reader_read_string` | `(r, VeltStr* out) -> u8` | owned, decoded (`\u` escapes incl. surrogate pairs; a lone surrogate → U+FFFD since UTF-8 cannot hold it) |
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
| syntax error (seen by the reader, or by `JSON.parseValue`) | `invalid JSON at <path>: <detail> (byte <offset>)`; for `parseValue` the path is that of the value being read when the error occurred (`$[1]`, `$.a`) |

With the reader's options: an unknown key (rejected) is `unknown field at <path>` (the decoder
puts the key in the path), and nesting past `max_depth` is
`JSON nested deeper than <max_depth> levels at <path> (byte <offset>)` (also from
`velt_rt_json_parse_value_with`). The prelude passes `max_depth` 128 unless the program sets
`maxDepth`.

`<detail>` is one of `unexpected end of input`, `unexpected character 'c'` (control characters as
`U+XXXX`), `expected ':'`, `expected ',' or '}'`, `expected ',' or ']'`, `expected string key`,
`invalid number`, `invalid escape`, `invalid \u escape`, `control character in string`,
`unexpected trailing characters`. `<offset>` is the byte offset in the source. Path syntax
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
| `velt_rt_json_value_len` | `(VeltJson v) -> u64` | array length, object member count, string byte length; else 0 |
| `velt_rt_json_value_as_f64` | `(VeltJson v) -> f64` | NaN if not a number |
| `velt_rt_json_value_as_bool` | `(VeltJson v) -> u8` | 1 only for `true` (use `kind` to tell `false` from non-bools) |
| `velt_rt_json_value_as_str` | `(VeltJson v, VeltStr* out) -> u8` | owned copy; 0 if not a string |
| `velt_rt_json_value_stringify` | `(VeltJson v, VeltStr* out)` | `JSON.stringify`: no whitespace, key order kept, numbers JS-formatted; null handle ⇒ `null` |
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
flight (a join's or a spawned task's result, a channel item being sent): it matches that value's
layout, which a swap doesn't change, and it goes away with the value.


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
first (ASCII `\d \w \b`, JS class literals; `crates/velt_rt/src/regex/syntax.rs`). Offsets are
byte offsets on character boundaries.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_regex_new` | `(const VeltStr* pattern, const VeltStr* flags, IoResult<VeltRegex>* out)` | flags from `dgimsuvy`, each once (`g y d u v` don't change matching); bad pattern/flags ⇒ `EINVAL` with a JS-style message (`Invalid regular expression: /p/f: …`) |
| `velt_rt_regex_free` | `(VeltRegex re)` | |
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
ALPN `h2` and `http/1.1`. This **supersedes the §7 note**: `velt_rt_http_fetch` now accepts
`https://` (other schemes fail with `ENOTSUP`) and negotiates HTTP/2 when the server offers it.
HTTP/1.1 server connections now support upgrades (`serve_connection_with_upgrades`).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_serve_tls` | `(const VeltStr* addr, const VeltHandler* h, const VeltStr* cert_pem, const VeltStr* key_pem) -> VeltFut*` | as `velt_rt_http_serve`, over TLS; certificate chain + PKCS#8/PKCS#1/SEC1 key; bad PEM or mismatch ⇒ `EINVAL`; handshakes time out after 10 s |
| `velt_rt_http_fetch_ca` | `(method, url, headers, u64 n_headers, body, const VeltStr* ca_pem) -> VeltFut*` | as `velt_rt_http_fetch`, also trusting the PEM CAs in `ca_pem` (`""` = none); clients are pooled per CA text |
| `velt_rt_http_resp_set_header` | `(VeltResp r, const VeltStr* name, const VeltStr* value) -> u8` | insert, replacing earlier values (e.g. the default `content-type`); 0 if invalid |
| `velt_rt_http_req_upgrade` | `(VeltReq r) -> u64` | key of the request's parked HTTP upgrade (0 = the request has no `Upgrade` header); an unclaimed upgrade is dropped when the handler's response is produced |

### 14.9 WebSockets (`velt:websocket`)

`VeltWs` is an `Arc`; its send and receive halves are locked separately (one task may wait in
`receive` while others send). Pings are answered automatically. No callbacks are stored.

```c
typedef struct { VeltWs ws; VeltResp response; } VeltWsAccept;                    // 16 bytes
typedef struct { uint32_t kind; uint32_t pad; VeltStr text; VeltBytes data; } VeltWsMessage; // 56
// kind: 0 = closed (no more messages), 1 = text (in `text`), 2 = binary (in `data`)
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_ws_accept` | `(u64 upgrade_key, const VeltStr* sec_websocket_key, IoResult<VeltWsAccept>* out)` | claims the parked upgrade (§14.8) and builds the `101` response (handshake headers) the handler must return; the connection's first operation waits for the upgrade; `EINVAL` if not an upgrade request |
| `velt_rt_ws_connect` | `(const VeltStr* url, const VeltStr* ca_pem) -> VeltFut*` | `IoResult<VeltWs>`; `ws://` / `wss://` (TLS as §14.8, extra CAs in `ca_pem`); HTTP/1.1 handshake |
| `velt_rt_ws_send_text` | `(VeltWs ws, const VeltStr* text) -> VeltFut*` | `IoResult<()>`; copied |
| `velt_rt_ws_send_binary` | `(VeltWs ws, const VeltBytes* data) -> VeltFut*` | `IoResult<()>`; copied |
| `velt_rt_ws_receive` | `(VeltWs ws) -> VeltFut*` | `IoResult<VeltWsMessage>` |
| `velt_rt_ws_close` | `(VeltWs ws, u32 code, const VeltStr* reason) -> VeltFut*` | `IoResult<()>`: close frame; closing a closed connection succeeds |
| `velt_rt_ws_free` | `(VeltWs ws)` | releases the handle |

### 14.10 HTML escaping (`velt:html`; stream std-2, additive)

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_html_escape` | `(const VeltStr* s, VeltStr* out)` | owned copy of `s` with `& < > " '` → `&amp; &lt; &gt; &quot; &#39;`, in one pass (output sized once); other bytes unchanged |

Stable hash (`velt:hash`, additive):

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_fnv1a64_str` | `(const VeltStr* s) -> u64` | `velt:hash`: 64-bit FNV-1a of the bytes; stable forever (std contract) |
| `velt_rt_fnv1a64_bytes` | `(const VeltBytes* data) -> u64` | the same over a `u8[]` |

### 14.11 SQLite (`velt:sqlite`; stream db, additive)

`crates/velt_rt/src/sqlite/` over `rusqlite` (SQLite compiled in: `bundled`; also
`column_decltype`). **Synchronous**: a typical statement takes microseconds, less than a
blocking-pool round trip. `VeltSqliteDb` is an `Arc` of `Mutex<Option<Connection>>` plus a failure
counter and the last error; `VeltSqliteStmt` is an `Arc` of `{ Arc<db>, sql }` that re-finds its
compiled statement in the connection's cache (`prepare_cached`, 256 entries) on every call, so
closing the database while statements exist is safe (they fail with `SQLITE_MISUSE`). The null
handle (a closed `Database` / `Statement` in std) fails the same way instead of crashing.

Errors: `err.code` is SQLite's **extended result code** (never 0; e.g. 2067
`SQLITE_CONSTRAINT_UNIQUE`), `message` SQLite's message. Parameter problems are `SQLITE_RANGE`
(25), a closed handle or several statements in one `prepare` `SQLITE_MISUSE` (21).

Parameters (`params`) are JSON text (`crates/velt_rt/src/db_json/params.rs`): `""` = none, an
object binds `:name` / `@name` / `$name` by name (missing member ⇒ `SQLITE_RANGE`, extra members
ignored), an array or a lone scalar binds by position (count must match). Integer literals in
i64 range ⇒ INTEGER (exact), other numbers ⇒ REAL, strings ⇒ TEXT, `true`/`false` ⇒ 1/0,
`null` ⇒ NULL, arrays of integers 0–255 (`u8[]`) ⇒ BLOB, nested objects ⇒ `SQLITE_RANGE`.
Rows are JSON text (`db_json/rows.rs`): `[{"col":value,...}]` keyed by column name, INTEGER as
exact digits, REAL in JS shortest form (non-finite ⇒ `null`), TEXT escaped like
`JSON.stringify`, BLOB as a number array, NULL as `null`; a 0/1 value in a column declared
`BOOL`/`BOOLEAN` is written as `false`/`true`.

```c
typedef struct { int64_t changes; int64_t last_insert_rowid; } VeltSqliteRun;   // 16 bytes
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_sqlite_open` | `(const VeltStr* path, u8 readonly, u8 create, u32 timeout_ms, IoResult<VeltSqliteDb>* out)` | `":memory:"`, a file path or a `file:` URI; `timeout_ms` = busy timeout |
| `velt_rt_sqlite_close` | `(VeltSqliteDb db, VeltErr* out)` | closes the connection (finalizing cached statements) and releases the handle; null ⇒ no-op |
| `velt_rt_sqlite_exec` | `(VeltSqliteDb db, const VeltStr* sql, VeltErr* out)` | `;`-separated statements, no parameters |
| `velt_rt_sqlite_pragma` | `(VeltSqliteDb db, const VeltStr* source, IoResult<VeltStr>* out)` | `PRAGMA <source>`: first column of the first row as text, `""` if none/NULL/blob |
| `velt_rt_sqlite_in_transaction` | `(VeltSqliteDb db) -> u8` | not in autocommit mode; 0 when closed |
| `velt_rt_sqlite_error_count` | `(VeltSqliteDb db) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_rt_sqlite_reset_error_count` | `(VeltSqliteDb db, u64 count)` | a nested `transaction` that rolled back hands its failure to its caller |
| `velt_rt_sqlite_last_error` | `(VeltSqliteDb db, VeltErr* out)` | the most recent failure as a failed status (code 0 if none) |
| `velt_rt_sqlite_prepare` | `(VeltSqliteDb db, const VeltStr* sql, IoResult<VeltSqliteStmt>* out)` | compiles exactly one statement into the cache |
| `velt_rt_sqlite_run` | `(VeltSqliteStmt stmt, const VeltStr* params, IoResult<VeltSqliteRun>* out)` | steps to completion (rows discarded) |
| `velt_rt_sqlite_query` | `(VeltSqliteStmt stmt, const VeltStr* params, u8 first_only, IoResult<VeltStr>* out)` | rows as a JSON array; `first_only` ⇒ the first row's object or `""` |
| `velt_rt_sqlite_note_error` | `(VeltSqliteStmt stmt, i32 code, const VeltStr* message)` | counts a failure std found (a row that does not decode) against the connection |
| `velt_rt_sqlite_stmt_free` | `(VeltSqliteStmt stmt)` | releases the handle; null ⇒ no-op |
| `velt_rt_sqlite_error_name` | `(i32 code, VeltStr* out)` | static name: known extended names (`SQLITE_CONSTRAINT_UNIQUE`, `SQLITE_BUSY_SNAPSHOT` …), else the primary one |

Not available on WebAssembly (no symbols in `libvelt_rt_wasm.a`: linking fails with undefined
`velt_rt_sqlite_*`).

### 14.12 Redis (std/redis; stream db, additive)

A RESP2 client written directly over tokio (`crates/velt_rt/src/redis/`), with TLS from the
shared rustls configuration of §14.8 (`ring`, webpki roots + extra PEM CAs). `VeltRedis` is an
`Arc` of one **multiplexed** connection: requests from any number of tasks are queued in order
and written in batches, and a reader task completes them in order; in-flight operations keep a
clone, so `close` is allowed any time. A failed connection fails the commands already written to
it (`ECONNRESET`); later commands reopen it (exponential backoff for up to 10 s, stream
linux-servers) or fail with the connect error. `VeltRedisSub` is an `Arc` of a dedicated
subscribed connection whose messages are pulled (no callbacks are stored, §13.5).

URLs: `redis://[[user]:password@]host[:port][/db]` or `rediss://…` (TLS); percent-decoded
credentials, `[v6]` hosts, `?db=N`. The handshake sends `AUTH` and `SELECT` as needed; connecting
(TCP + TLS + handshake) times out after 10 s (`ETIMEDOUT`). A bad URL is `EINVAL`.

Errors: I/O failures use the §3 codes. An **error reply** from the server uses code **100**
with the reply text as the message (`WRONGTYPE Operation against …`); std/redis takes its first
word as `RedisError.code`. A reply that is not valid RESP is `EILSEQ`.

```c
typedef struct { VeltBytes tags; VeltArray<i64> nums; VeltStrArray strs; } VeltRedisReply; // 72
// One entry per node in pre-order (a node, then its children). tags: 0 nil, 1 status,
// 2 error, 3 int, 4 bulk string, 5 array; nums: the int value or the array's length (else 0);
// strs: the text of status/error/string nodes (else ""; invalid UTF-8 becomes U+FFFD).
typedef struct { uint32_t kind; uint32_t pad; VeltStr channel; VeltStr message; VeltStr pattern; }
    VeltRedisMessage;                                                                     // 80
// kind: 0 = closed (after close), 1 = message, 2 = pattern message (pattern set)
```

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_redis_connect` | `(const VeltStr* url, const VeltStr* ca_pem) -> VeltFut*` | `IoResult<VeltRedis>`; `ca_pem` `""` = built-in roots only |
| `velt_rt_redis_duplicate` | `(VeltRedis c) -> VeltFut*` | `IoResult<VeltRedis>`: a new connection opened like `c` |
| `velt_rt_redis_close` | `(VeltRedis c)` | releases the handle |
| `velt_rt_redis_command` | `(VeltRedis c, const VeltStrArray* args) -> VeltFut*` | `IoResult<VeltRedisReply>`; `args[0]` is the command (copied); a top-level error reply fails with code 100; empty `args` ⇒ `EINVAL` |
| `velt_rt_redis_pipeline` | `(VeltRedis c, const VeltStrArray* args, const VeltArray<u64>* counts, u8 atomic) -> VeltFut*` | `IoResult<VeltRedisReply>`: `args` split into commands of `counts[i]` args, sent in one write; the reply is an array with one node per command, error replies as tag 2. `atomic`: wrapped in `MULTI`/`EXEC`, the reply is `EXEC`'s array and an aborted transaction fails with code 100 (`EXECABORT …`, naming the first rejected command) |
| `velt_rt_redis_subscribe` | `(VeltRedis c, const VeltStr* url, const VeltStr* ca_pem, const VeltStrArray* names, u8 patterns) -> VeltFut*` | `IoResult<VeltRedisSub>`: a new connection to where `c` is connected (`c != 0`) or to `url`; `SUBSCRIBE` (`PSUBSCRIBE` when `patterns`) resolves once every name is confirmed |
| `velt_rt_redis_sub_next` | `(VeltRedisSub s) -> VeltFut*` | `IoResult<VeltRedisMessage>`: the next message; kind 0 once closed; a connection lost to the server ⇒ `ECONNRESET` |
| `velt_rt_redis_sub_change` | `(VeltRedisSub s, const VeltStrArray* names, u8 patterns, u8 subscribe) -> VeltFut*` | `IoResult<()>`: `(P)SUBSCRIBE` (`subscribe` 1) or `(P)UNSUBSCRIBE` (0), resolved once confirmed; no names = no-op |
| `velt_rt_redis_sub_close` | `(VeltRedisSub s)` | pending and later `next` calls resolve with kind 0; releases the handle |

Not available on wasm (no sockets): the symbols are not defined by `velt_rt_wasm`.

### 14.13 PostgreSQL (`velt:postgres`; stream db, additive)

`crates/velt_rt/src/postgres/` over `tokio-postgres` (`default-features = false`, `runtime`), connected over the runtime's own socket and wire stream (§14.18).
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
format (parsed by the server); Null ⇒ NULL; anything else ⇒ `EINVAL`.
Rows are JSON text (`db_json::RowWriter`): int2/int4/int8/oid exact, float4/float8 (non-finite
⇒ `null`), numeric ⇒ string of its digits, bool, text types/enums ⇒ string, uuid ⇒ string,
date/time/timestamp ⇒ ISO 8601 string, timestamptz ⇒ ISO 8601 UTC with `Z`, json/jsonb ⇒ the
document, bytea ⇒ number array, arrays ⇒ nested arrays, domains ⇒ base type, NULL ⇒ `null`;
other column types ⇒ `ENOTSUP` (checked before any row).

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_pg_connect` | `(const VeltStr* url) -> VeltFut*` | `IoResult<VeltPgClient>`; URL or libpq `key=value` string |
| `velt_rt_pg_query` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params, u8 first_only) -> VeltFut*` | `IoResult<VeltStr>`: rows as a JSON array; `first_only` ⇒ the first row's object or `""` |
| `velt_rt_pg_execute` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | `IoResult<i64>`: rows affected/returned |
| `velt_rt_pg_batch` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `VeltErr`: `;`-separated statements, simple protocol, not cached |
| `velt_rt_pg_begin` | `(VeltPgClient c) -> VeltFut*` | `IoResult<u32>`: new level (1 = `BEGIN`, n > 1 = `SAVEPOINT velt_tx_n`) |
| `velt_rt_pg_end` | `(VeltPgClient c, u32 depth, u8 commit) -> VeltFut*` | `VeltErr`: commit / roll back level `depth`, which must be the innermost (`EINVAL` otherwise); the level ends whatever the outcome |
| `velt_rt_pg_depth` | `(VeltPgClient c) -> u32` | open levels; 0 when closed |
| `velt_rt_pg_error_count` | `(VeltPgClient c) -> u64` | failed operations so far (incl. `note_error`) |
| `velt_rt_pg_reset_error_count` | `(VeltPgClient c, u64 count)` | forget failures after `count` |
| `velt_rt_pg_error_after` | `(VeltPgClient c, u64 count, VeltErr* out)` | the first failure after `count` failures (code 0 if none) |
| `velt_rt_pg_note_error` | `(VeltPgClient c, const VeltStr* code, const VeltStr* message)` | counts a failure std found (`EMISMATCH` row decoding) |
| `velt_rt_pg_close` | `(VeltPgClient c)` | closes (or returns a pooled connection to its pool unless broken or in a transaction) and releases the handle; null ⇒ no-op |
| `velt_rt_pg_pool_new` | `(const VeltStr* url, u32 max, IoResult<VeltPgPool>* out)` | parses the URL and reads `sslrootcert` now; connections open on demand; `max == 0` ⇒ 10 |
| `velt_rt_pg_pool_connect` | `(VeltPgPool p) -> VeltFut*` | `IoResult<VeltPgClient>`: a dedicated connection (holds a permit) until `velt_rt_pg_close` |
| `velt_rt_pg_pool_query` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* params, u8 first_only) -> VeltFut*` | as `velt_rt_pg_query` on a borrowed connection |
| `velt_rt_pg_pool_execute` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* params) -> VeltFut*` | as `velt_rt_pg_execute` |
| `velt_rt_pg_pool_batch` | `(VeltPgPool p, const VeltStr* sql) -> VeltFut*` | as `velt_rt_pg_batch` |
| `velt_rt_pg_pool_idle` | `(VeltPgPool p) -> u32` | idle connections |
| `velt_rt_pg_pool_end` | `(VeltPgPool p)` | closes idle connections, fails later acquires with `ECLOSED`, releases the handle; null ⇒ no-op |

Linking: tokio-postgres' `whoami` needs `-framework SystemConfiguration -framework
CoreFoundation` on macOS and `secur32.lib` on Windows (added to `velt_link`'s native library
lists). Not available on WebAssembly (no `velt_rt_pg_*` symbols in `libvelt_rt_wasm.a`).

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
| `velt_rt_pg_copy_from` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyIn>`: `COPY … FROM STDIN` started |
| `velt_rt_pg_copy_write` / `_write_bytes` | `(VeltPgCopyIn w, const VeltStr*/VeltBytes* data) -> VeltFut*` | `IoResult<()>`; copied, batched into ~4 KiB messages |
| `velt_rt_pg_copy_end` | `(VeltPgCopyIn w) -> VeltFut*` | `IoResult<i64>` rows copied; releases `w` |
| `velt_rt_pg_copy_abort` | `(VeltPgCopyIn w)` | releases `w` unfinished: the server aborts the copy |
| `velt_rt_pg_copy_to` | `(VeltPgClient c, const VeltStr* sql) -> VeltFut*` | `IoResult<VeltPgCopyOut>`: `COPY … TO STDOUT` started |
| `velt_rt_pg_copy_read` / `_read_bytes` | `(VeltPgCopyOut r) -> VeltFut*` | `IoResult<VeltStr / VeltBytes>`: every chunk received so far (≥ 1, up to ~64 KiB), empty at the end |
| `velt_rt_pg_copy_close` | `(VeltPgCopyOut r)` | releases `r` (unread data is discarded) |

### 14.16 Fast random numbers (`velt:random`; stream linux-servers, additive)
| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_prng_f64` | `() -> f64` | uniform `[0, 1)`, 53 bits; per-thread wyrand seeded from §14.1 |
| `velt_rt_prng_range` | `(i64 min, i64 max) -> i64` | uniform `[min, max)` (unbiased); `min` if `max <= min` |


### 14.17 Streamed response bodies (`Response.stream`; stream tsx, additive)
A `VeltResp` body is now either complete (the §7 setters: unchanged, still sent with an exact
`Content-Length`, no extra allocation) or **streamed**: `velt_rt_http_resp_stream_open` replaces
the body with the receiving end of a bounded channel (8 chunks) and returns a writer,
`VeltRespWriter`, a registry key (§3.2, `u64`). A streamed body has no known length, so hyper
sends it with chunked transfer encoding (HTTP/1.1) or DATA frames (HTTP/2) and never computes a
`Content-Length`; the status and headers go out when the handler returns the response, before
the first chunk. The writer is filled by Velt code that keeps running after the handler returned
(std starts it as a stored promise of the handler's task, §1.1); no code pointers are stored
(§13.5).

Writes append a copy to the writer's buffer (`BytesMut`) and never wait; a flush hands the
buffer to the body as one chunk, waiting for channel room (**backpressure**: a producer faster
than its client waits in `flush`). A buffer that reaches 16 KiB is also handed over by a write
when the channel has room. Chunks keep their write order even when copies of the handle flush
concurrently. Once the client has gone away (hyper dropped the body) or the writer ended, writes
and flushes return 0 and discard their data. A writer that is neither closed nor aborted keeps
its response open.

| Symbol | Signature | Notes |
|---|---|---|
| `velt_rt_http_resp_stream_open` | `(VeltResp r) -> VeltRespWriter` | body becomes a stream; default `content-type: text/plain; charset=utf-8` unless one is set; opening again detaches the earlier writer (its writes return 0). A bodiless status (1xx, 204, 304) keeps the empty body and gets no `content-type`; the writer's writes return 0 |
| `velt_rt_http_resp_stream_write` | `(VeltRespWriter w, const VeltStr* text) -> u8` | buffers a copy; 0 once ended, client gone, or `w` released |
| `velt_rt_http_resp_stream_write_bytes` | `(VeltRespWriter w, const VeltBytes* data) -> u8` | the same for `u8[]` |
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
| `velt_rt_pg_query_batch` | `(VeltPgClient c, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | `IoResult<VeltStr>`: a JSON array, per execution: `mode` 0 its rows (`[[{…},…],…]`), 1 its first row or `null` (`[{…},null]`), 2 its rows affected (`[1,0]`) |
| `velt_rt_pg_pool_query_batch` | `(VeltPgPool p, const VeltStr* sql, const VeltStr* sets, u8 mode) -> VeltFut*` | as `velt_rt_pg_query_batch`, on one pooled connection |
