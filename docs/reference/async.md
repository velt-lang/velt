# Async and concurrency

## Async functions

`async function f(): Promise<T>` compiles to a state machine. `await` is allowed only inside
`async` functions and arrows (``` `await` is only allowed inside async functions ```). Async
methods and async arrows work the same way. `async function main()` runs on the runtime, a
multi-threaded tokio executor with one worker per core.

An async arrow passed where a `void` function is expected (`onClick?: () => void`, a
`(() => void) | null` parameter) is accepted, as in TypeScript: each call starts the arrow's
promise, which runs to completion on its own (a [dropped promise](#promises) is not cancelled).
Against a union of function types an arrow takes the member that fits, as TypeScript's contextual
typing does: an async arrow the one returning a promise (`(() => void) | (() => Promise<void>)`), a
sync arrow one returning a value before a `void` one, so its result is kept
(`(() => void) | (() => number)` given `() => 42` returns `42`), and a promise-returning member
last.

`await` on a value that may or may not be a promise (`T | Promise<T>`, the result of a sync-or-async
callback, or `Promise<T> | null`) awaits a promise and gives any other value as it is, as in JS:
`await h(1)` where `h: (x: number) => Resp | Promise<Resp>` is a `Resp`. A promise that may be
missing keeps its nothing: `await r["k"]` of a `Record<string, Promise<number>>` (or
`await m.get(k)`) is a `number | undefined`, and so is `await x` with
`x: number | Promise<void>`.

An async function that returns a promise (`return Promise.resolve(x)`, `return load()`) or a value
that may be one (`return fn()` with `fn: () => T | Promise<T>`) adopts it, as in JS: its own
promise settles as the returned one does. The steps are Node's: the promise is adopted after the
function has left every scope it returns from (`finally` blocks run, `using` values are disposed
and the iterator of a `for…of` is closed first, and an enclosing `catch` does not see the
rejection), and one microtask later than `return await` would settle. A `return` that a
`finally` block cancels (with `continue`, `break` or another `return`) adopts nothing. A plain
value of a `T | Promise<T>` is returned at once. Without a written result type the function's result is the promise's value type, as in
TypeScript: `const f = async (n: number) => load(n)` with `load(n): Promise<number>` is a
`(n: number) => Promise<number>`.

```ts
async function load(n: number): Promise<number> {
  return n * 2;
}

async function viaLoad(n: number): Promise<number> {
  return load(n);
}

async function main() {
  console.log(await viaLoad(21));
}
```

An async arrow passed where the function type returns a union with one promise member
(`(n: number) => View | Promise<View>`, or `Promise<T> | null`) returns that promise, as in
TypeScript: its body returns `View`. The same holds for such an arrow as the value of a
module-level constant or a field initializer (`const load: () => number | Promise<number> = async
() => 1;`). Such a result is told apart with `r instanceof Promise`
([unions](types.md#union-types)):

```ts
type View = () => string;

async function render(setup: (n: number) => View | Promise<View>): Promise<string> {
  const r = setup(1);
  const view = r instanceof Promise ? await r : r;
  return view();
}

async function main() {
  console.log(await render((n) => () => `sync ${n}`));
  console.log(await render(async (n) => () => `async ${n}`));
}
```

## Promises

Promises behave like JavaScript's, at Rust's cost:

| Code | Behavior | Cost |
|---|---|---|
| `await f()` | `f` runs in place | nothing: no task, no allocation |
| `const p = f()` … `await p` | `f` starts now, like JS: it runs until its first `await`, then concurrently with the caller | one small allocation |
| `f();` | compile error (floating promise): write `await f()` or `spawn(f())` | — |
| `Promise.all` / `race` / `allSettled` / `any` | concurrent | one allocation per promise |

- A started promise runs on its creator's task, never at the same time as the creator or the
  task's other promises (JavaScript's single-threaded model), so no thread-safety rules apply to
  it. Only `spawn` puts work on another core.
- `await` takes Node's steps: the code after it runs one microtask after the promise settles
  (one microtask later when it already has), behind the callbacks and other `await`s queued before it, so
  promises started side by side interleave as in Node (`const a = f(); const b = g();` runs `f`
  and `g` up to their first `await`, then the caller's next line, then their continuations in
  turn). Awaiting an async call takes its step when the call returns. A program that never
  starts a promise, chains a callback or settles one with a stored `resolve` can't tell the
  steps apart, and its `await`s cost what a plain call does.
- Timers of one task fire in order: by deadline, then in the order the `sleep` calls were made.
  So the started promises (and the task itself) waiting for timers that are due together resume
  in the order their timers were created, like `setTimeout` callbacks in Node. A timer that is
  already due when it is first waited for (`sleep(0)`, or a `sleep` awaited after its delay
  passed) also waits for its turn: the code after it never runs before the synchronous code
  that started it, nor before timers due earlier. `sleep`s of different tasks have no order
  between them, since the tasks run in parallel; `setTimeout`, `setInterval` and `setImmediate`
  callbacks do, whichever task created them: they run in Node's order, one list per delay
  ([velt:timers](../std/timers.md)).
- **A dropped promise is not cancelled**: a stored promise that is never awaited still runs to
  completion (its result is dropped), and the program waits for it before exiting, like Node
  waits for pending work. A synchronous `main` that starts promises, timers or microtasks runs
  on a task like an `async main`, so they run as in Node: up to their first `await` when
  created, and what they queued once `main` returns. A promise created outside async code (for example in a synchronous
  `main`) starts when it is awaited or spawned.
- After `main` returns, the process also waits for *handles*, as in Node: a listening
  [server](../std/http.md) and a pending [timer](../std/timers.md) (`setTimeout`,
  `setInterval`) unless it is `unref()`ed. A spawned task is not a handle: tasks still running
  when nothing else keeps the process alive end with it. `process.exit()` and an uncaught error
  end the process at once.
- An async call owns its arguments: an argument variable used again afterwards is shared with
  the promise (objects) or copied (numbers, strings), otherwise moved, because the promise may
  outlive the caller's frame.
- **A promise is a shared value**, as in JavaScript: `const q = p`, storing a promise in a
  `Map`, an array or a field and reading it out (`m.get(k)`, `arr[i]`, `entry.p`), and handing
  it to `then` while keeping it, all give other holders of the same promise. Every holder may
  await it, and each gets the result: an object result is the same object for every awaiter
  (`(await p) === (await q)`), a string or a number is copied. A rejection reaches every
  awaiter. The awaiters of a pending promise resume in the order they started waiting, as in
  Node. Dropping a holder never cancels the promise; it is reported as an unhandled rejection
  only when every holder dropped it without awaiting it. A promise that is never shared costs
  what it did before; a shared one costs a small side record and one share of the result per
  `await`. A shared promise is driven on the task that shares it:
  - A promise passed as an argument of a spawned call (`spawn(f(p))`) moves to the new task, so
    using it afterwards is an error; pass another holder (`const q = p; spawn(f(q))`) to await
    it on both tasks.
  - Another holder of a shared promise that goes to another task (`const q = p; spawn(f(q))`,
    `spawn(q)`, a promise read from a `Map`, `ch.send(q)`, a [timer](../std/timers.md) callback
    capturing it, which runs as a task of its own, a `Promise.all` or `Promise.race` over it) is
    awaited there through a copy. So is one inside a value that goes there while it is still
    used here (an object, an array or a closure passed to a spawned call or sent over a
    channel): the task gets a copy of the value, whose promises are other holders. The promise
    still runs on its own task, and the other task gets the result transferred like a `spawn`
    argument (moved, or copied while the first task still uses it), so `(await p) === (await q)`
    holds only on one task. On the other task the holder is a promise of that task: it may be
    shared again there, and handed on to a third task in turn.
  - The holders of a pending promise are queued when it settles, in the order they started
    waiting, as Node queues a promise's reactions: a `Promise.race` over a holder takes it before
    a promise that settles later.
- A `using` variable may be the receiver or an argument of an async call only when the call is
  awaited where it is made (`await r.read()`): it is disposed at the end of its block, which a
  stored or returned promise could outlive (``an async call that keeps it must be awaited
  here``), and it cannot go to `spawn` at all. An `await using` variable may be shared with a
  stored promise: the block awaits its `[Symbol.asyncDispose]()` when it ends.
- Values handed to `spawn` (and captured by an HTTP handler, sent over a channel, or settled on
  a promise from another task) go to another thread. What the program no longer references
  anywhere else moves as it is; an object it still shares is deep-copied for the task (like a
  structured clone), so threads never share reference counts. As with a structured clone, an
  object the value reaches more than once is copied once (`p.x === p.y` still holds in the
  task) and a cycle is copied as a cycle. That includes the receiver of
  `spawn(obj.method())` (also through a base-class reference or an interface value) and what a
  closure or interface value passed to the task reaches. A closure the caller still uses
  afterwards is copied too, with what it captures (also a variable it assigns), so the task and
  the caller each run their own copy. A closure handed on for the last time that captures only
  values nothing else references (an HTTP handler capturing a disposable resource, say) goes to
  the task as it is. `spawn(async () => …)` and `spawn((async () => …)())` hand the captured
  values themselves to the task, so a captured resource the program no longer uses moves and
  its `[Symbol.dispose]()` runs once, on the task. Each call of an async closure that may run
  on another thread otherwise gets its own copy of what the closure captured, except a resource
  without `clone()`, which the call shares with the closure (it is released once, after both).
  An async closure that stays on its task shares what it captured with every call instead, as
  in JavaScript ([Functions](functions.md#captures)). So `spawn(f())` through an
  async closure value `f` gives the task a copy of `f`, and stops the program
  (``panic: cannot copy …``) when `f` captured such a resource.
- A value owning a `[Symbol.dispose]` resource is copied by its class's own `clone()` method
  ([Classes](classes.md)), so each copy releases its own resource; what the returned object
  still shares with the original (a shallow `clone()`) is deep-copied in turn, and a `clone()`
  that returns `this` stops the program. Classes without a resource are copied field by
  field, whatever their `clone()` does. One without `clone()` cannot
  be copied: passing it to `spawn` and using it afterwards is an error ("`r` is still used
  after `spawn`, so the task would get a copy, …"), and so is passing one an object still
  holds (`spawn(serve(this.conn))`: "`this.conn` stays where it is held, …"); pass the last
  reference, give the class a `clone()`, or share it with `shared(new Mutex(conn))` (a class
  is shared behind a [`Mutex`](#thread-safety)). The same holds for a value sent on a
  channel (`ch.send(r)`, `ch.trySend(r)`, or a function that passes its parameter on to one:
  "`r` is still used after `send`, so the receiving task would get a copy, …"), and for such
  a value inside an object, array or tuple literal built for the task or the channel
  (`ch.send({ conns })`). A function that sends its parameter only on some paths is checked
  as if it always sent it, like a `spawn` inside an `if`. When another reference is only found at run time
  (the value is also in an array, say), the program stops with ``panic: cannot copy a `Conn`
  for another thread …``.

## Combinators

- `Promise.all(ps: Promise<T, E>[]): Promise<T[], E>`: the results, in order; rejects as soon
  as one promise rejects.
- `Promise.race(ps): Promise<T, E>`: the first to settle; the others keep running to
  completion.
- `Promise.allSettled(ps)`: a `PromiseSettledResult<T, E>[]`, where each element is
  `{ status: "fulfilled"; value: T } | { status: "rejected"; reason: E }`.
- `Promise.any(ps): Promise<T>`: the first to fulfill; `AggregateError` when all of them reject
  (or the array is empty).
- `Promise.withResolvers<T, E>()`: a pending promise with its `resolve` and `reject`
  ([below](#promisewithresolvers)).
- `Promise.resolve(value): Promise<T>`: a promise fulfilled with `value` (`Promise.resolve()`
  is a `Promise<void>`; a promise passed in is returned as it is, as in JS). `T` comes from the
  argument, the type argument (`Promise.resolve<number>(p)`) or the expected type.
- `Promise.reject(reason: E): Promise<T, E>`: a promise rejected with `reason`. `T` comes from
  the type argument (`Promise.reject<string>(e)`) or the expected type, else it is `never`.

```ts
class Failed extends Error {}

async function main() {
  console.log(await Promise.resolve(5)); // 5
  try {
    await Promise.reject(new Failed("no"));
  } catch (e) {
    console.log(e.message); // no
  }
}
```

All promises in one call must have the same value type (**Planned**: different ones, #754).
Their error types may differ: an array of promises rejecting with `A` and with `B` (or never
rejecting) is an array of promises rejecting with `A | B`, and the combinator rejects with
that. Like in JS, every promise passed to a
combinator is *handled*: one that loses (or is left behind) and rejects later has its error
dropped, not reported as uncaught, so a timeout written as a rejecting promise in a
`Promise.race` is fine once the work won (`Promise.allSettled` awaits every promise itself).
Losing promises that already started, such as calls of async functions, run to completion;
a runtime operation that loses, such as `sleep(ms)` or an I/O call, is cancelled. As in JS, a
loser that can go on when the combinator settles (a sibling resolved what it awaits) takes that
step before the code after the `await`, while one waiting for a timer, I/O or `yieldNow()` goes
on later, when that happens. So a loop of combinators that settle at once finishes such losers
as it goes, like Node. A combinator
kept as a value is itself a stored promise: if nobody awaits it, its own rejection is reported
as uncaught.

## `then`, `catch` and `finally`

Promises have JavaScript's callback methods. Each returns a new promise, which starts at once
like any stored promise:

- `p.then(f)`: `f(value)` once `p` fulfills; a rejection passes through to the new promise.
- `p.then(f, g)`: `g(reason)` when `p` rejects (both callbacks give the same type).
  `p.then(null, g)` and `p.then(undefined, g)` are `p.catch(g)`, and `p.then(f, null)` is
  `p.then(f)`, as in JS.
- `p.catch(g)`: `g(reason)` when `p` rejects; a value passes through, so `g` gives the
  promise's value type, or nothing: `p.catch(() => {})` (or an `async` handler) is a
  `Promise<T | undefined>`, TypeScript's `Promise<T | void>`, which gives `p`'s value or
  `undefined` once `g` handled a rejection (**Planned**: a `catch` that changes the value
  type otherwise, `T | U`).
- `p.finally(h)`: `h()` either way, then the value or the reason passes through.

A callback that returns a promise is awaited (flattened): `p.then((id) => load(id))` is a
promise of what `load` resolves to, settled in as many steps as in Node (one to adopt the
returned promise, one after it settles). The new promise rejects with what `p` rejects with (unless
it is handled) and with what the callback throws; a callback may throw a different error type
only when `p` cannot reject (**Planned**: both, as a union). A `then` chain kept as a statement
(`p.then(f);`) is not a floating promise: it runs to completion, and a rejection nobody handles
is reported as uncaught, like a dropped promise, once the callbacks queued before it have run
(as in Node). `queueMicrotask(f)` runs `f` the same way. Both need async code around them:
in synchronous code (a synchronous `main`, or top-level code without an `await`) the chain
starts only when awaited or spawned, like any [dropped promise](#promises), so a chain kept as a
statement there and `queueMicrotask` never run (**Planned**: run them as Node does, #846).

A promise may have several consumers, as in JavaScript: `p.then(f); p.then(g); await p` runs
`f`, then `g`, then the code after the `await`, each with the value (see [Promises](#promises)).

Callbacks run in Node's order: after the code that is running, in the order they were queued
(each task has a microtask queue), and before timers and I/O: a `setTimeout` callback never
runs before the callbacks queued by the task that scheduled it. `finally` passes the value or
the reason on two steps after its callback, as in Node, and `Promise.allSettled` and
`Promise.any` settle in the step Node's do. Chains interleave as in Node
(`a.then(f1).then(f2)` next to `b.then(g1).then(g2)` runs `f1 g1 f2 g2`). Code after an `await`
goes through the same queue, so `p.then(f); await q;` runs `f` first, as in Node. A `resolve` or
`reject` called later (kept from `Promise.withResolvers()` or a `new Promise` executor) queues
the callbacks and `await`s waiting for that promise right away, as Node does.

Where the order still differs from Node's: `Promise.all` and `Promise.race` settle in fewer
steps (they keep `await` on them fast), which matters only for callbacks and `await`s running
next to them. Async generators take Node's steps (a `yield` awaits its value, `return()` awaits
before it closes the body), except for calls queued behind a running one: such a call starts one
step later than in Node, and the code after an `await` of an earlier call can run after it
rather than before (so a queued `return()` may run its `finally` blocks first). A promise settled from another task resumes its waiters at that
task's next step (the tasks run in parallel; there is no order between them). On WebAssembly, a
`resolve` called later resumes its waiters at the executor's next step, and so does a shared promise
that settles (its other awaiters can run after callbacks queued later than in Node).

```ts
class Failed extends Error {}

async function load(id: number): Promise<string, Failed> {
  if (id < 0) {
    throw new Failed(`no item ${id}`);
  }
  return `item ${id}`;
}

async function main() {
  load(-1)
    .catch((e) => e.message)
    .then((m) => console.log(m));
  const shown = load(1)
    .then((item) => item.toUpperCase())
    .finally(() => console.log("loaded"));
  queueMicrotask(() => console.log("queued"));
  console.log("first");
  console.log(await shown);
  // first, queued, no item -1, loaded, ITEM 1
}
```

## Async iteration

`for await (const x of src)` awaits each element of an async iterable
([Control flow](control-flow.md#for-await)), and an `async function*`
([async generator](functions.md#async-generators)) produces one, awaiting and yielding as it
goes:

```ts
async function* lines(texts: string[]): AsyncGenerator<string> {
  for (const t of texts) {
    await sleep(1);                        // e.g. read the next line
    yield t;
  }
}

async function count(): Promise<i64> {
  let n = 0;
  for await (const line of lines(["a", "b"])) {
    console.log(line);
    n += 1;
  }
  return n;
}
```

| Code | Behavior | Cost |
|---|---|---|
| `for await (const x of agen(a))` | the generator runs inside the caller, step by step | nothing: its state is part of the caller's, no allocation per item |
| `const g = agen(a)` … `await g.next()` | an `AsyncGenerator<T>` object | one allocation for the generator; each `next()` is a direct call |
| `for await` over an `AsyncIterable<T>` value | `next()` through the interface | one allocation per `next()` (its promise) |

- Leaving a `for await` early awaits the iterator's `return()`, which runs the generator's
  `finally` blocks (they may `await`).
- Calls of `next()` and `return()` on a stored generator are queued like JS's: one started
  while another is running waits for its turn, and each promise gets its own step, in call
  order (`const p1 = g.next(); const p2 = g.next();` gives `p1` the first value whichever is
  awaited first). A call whose promise is dropped or loses a race still takes its turn. The
  standard library's async iterators (a channel's, a socket's) are async generators and
  behave the same. Waiting for a turn costs nothing unless calls overlap.
- An async generator, like a started promise, belongs to the task that created it: passing one
  to `spawn` (or a channel) or capturing one in an async closure is a compile-time error. A task
  dropped while it is suspended inside a `for await` drops the generator with it (cancellation:
  its values are dropped; `finally` blocks that would `await` do not run).
- A class whose `[Symbol.asyncIterator]` is an async generator method (`async
  *[Symbol.asyncIterator]()`) is iterated like a direct call: its state is part of the caller's.

### Std sources

The standard library's streams are async iterables, so a consumer is a `for await` loop. Each
one also keeps its pull method (`receive()`, `readLine()`, `next()`, `tick()`), and leaving a
loop early leaves the source open where it was: a channel or a socket may have other users, so
ending the stream is always an explicit `close()` (or `stop()`).

| Source | Loop | Ends when |
|---|---|---|
| [`Channel<T>`](../std/channel.md) | `for await (const job of jobs)` | the channel is closed and drained |
| [`FileReader`](../std/fs_stream.md) | `for await (const line of reader.lines())` | end of file |
| [standard input](../std/stdin.md) | `for await (const line of lines())` | end of input |
| [`WebSocket`](../std/websocket.md) | `for await (const msg of ws)` | the peer closed the connection |
| [`RedisSubscriber`](../std/redis.md) | `for await (const m of sub)` | `sub.close()` |
| [`Ticker`](../std/timers.md) | `for await (const n of ticker)` | the ticker is stopped |
| [postgres `CopyReader`](../std/postgres.md) | `for await (const chunk of reader)` | the end of the `COPY` |

```ts
import { channel, Channel, ChannelClosed } from "velt:channel";

async function produce(out: Channel<i64>): Promise<void> throws ChannelClosed {
  for (let i = 1; i <= 3; i++) {
    await out.send(i * i);
  }
  out.close();
}

async function main() {
  const squares = channel<i64>(1);
  const producer = spawn(produce(squares));
  for await (const n of squares) {
    console.log(n); // 1, 4, 9
  }
  await producer;
}
```

Iterating these costs what the pull loop costs: their `[Symbol.asyncIterator]` methods are
async generators, which a direct `for await` runs inside the caller (no allocation per value).

## Tasks

`spawn(p)` returns a `Promise<T>` join handle. `spawn(f())`, or `spawn(async () => { … })`, runs
`f` as a task of its own on any core, and a spawned task runs even if nobody awaits it.
`spawn(c ? f(x) : g(y))` spawns the call the condition picks, like
`c ? spawn(f(x)) : spawn(g(y))`. A promise that already started (`const p = f(); spawn(p)`)
stays on the task that started it. A promise that goes to another task (`spawn(p)`,
`spawn(g(p))`, a channel), and a task's own result awaited through its join handle, deliver
the value there as a transferred one: moved if the task that produced it no longer references
it (a promise it started still may), else a copy made where it was produced, so two tasks never
use one object (also on single-threaded WebAssembly, where tasks share the one thread).

`spawn(p, { worker: i })` pins the task to worker `i` (`0` to `workerCount() - 1`, from
[`velt:task`](../std/task.md#task-placement)): it starts there and never moves. `p` must be a
call or a task body (not a promise that already started); an index out of range is a panic at
the spawn. A program that pins a task runs on the per-core scheduler, where every task stays on
the worker it started on, so a caller pinned next to the task it talks to never wakes another
thread. Calling `workerCount()` or `currentWorker()` alone doesn't change the scheduler.

Tasks exchange values through channels ([`velt:channel`](../std/channel.md)): typed,
bounded or unbounded queues where `send` waits while a bounded channel is full.

Output from different tasks interleaves line by line: a `console.log` line is never split by
another task's output. Lines a task prints before it hands work to another task come before
anything that task prints in response. Hand-offs are:

- `spawn`;
- a channel `send`, a `close`, or a `receive` that frees room in a bounded channel;
- settling a `new Promise` or `Promise.withResolvers` promise;
- `abort()`;
- a child leaving a task scope.

Hand-offs through shared state (`shared`, a `Mutex`) and through timers are not covered: to keep
such lines in order, print them from one task. Writing to stderr flushes stdout first.

Built-ins: `sleep(ms)`, `yieldNow()`, `performance.now(): f64` (monotonic milliseconds) and
`Date.now(): i64`. Timers and intervals are in [`velt:timers`](../std/timers.md).

## Cancellation

Cancellation is cooperative, with TypeScript's `AbortController` / `AbortSignal`
([`velt:task`](../std/task.md)): `abort()` marks the signal, and code that takes one checks it
(`throwIfAborted()`) or races its work against `signal.whenAborted()` (put the wait itself in
the `Promise.race`, so it is dropped when the work wins). A started promise is never cancelled
behind the program's back, so `finally` blocks and `using` disposal run as usual.
`timeout(p, ms)` rejects with `TimeoutError` when `p` is too slow (`p` is then abandoned like a
`Promise.race` loser: a running call keeps running, like JS).
`taskScope(async (scope) => …)` is structured concurrency: it settles only after every task
started with `scope.spawn`, fails with the first error of the body or a child, and that error
aborts `scope.signal` so the siblings stop.

A task the runtime drops is cancelled at its current suspension point: the values it owns are dropped (`using` resources are disposed),
`finally` blocks don't run, and its unfinished local promises are cancelled with it.

## Thread safety

Thread safety is checked at compile time: an async closure that may run on another thread (it
is spawned, handles HTTP requests, goes into `shared(...)` or a `Mutex`, is sent on a channel,
settles a promise, or is passed directly to a function value, an interface method or an
overridden method, which may keep it; also when it gets there through a variable, parameter,
capture or object holding it) must not modify what it captured. The error names both places:

```text
error: this async closure modifies captured `count`, so it must stay on the task that created it
  --> main.vlt:9:9: but it reaches `spawn` here
```

An async closure that never leaves its task may: its calls run as started promises on that
task, one at a time between `await`s, so they share what it captured as in JavaScript
([Functions](functions.md#captures)). Share state between tasks with:

- `shared(x)`, which gives a `shared<T>`: an atomically reference-counted value. Assigning,
  passing or capturing it adds a reference (so does `.clone()`); it is never deep-copied. For
  64-bit integers, `.add(n)`, `.get()` and `.set(v)` are atomic; `.add(n)` returns the new value.
  Any other `shared<T>` (one that holds no `Mutex`) never changes, so every task reads it at
  once, without a lock, like the `T` it holds: `s[i]` and `s.length` on a string, array or
  typed array, `for (const x of s)` (also over a `Map`, `Set` or typed array), a field or getter (`s.name`, `s.size` of a `Map`), `T`'s methods
  (`s.includes(x)`, `s.get(k)`), and `s` passed where a `T` is expected (`count(s)` for
  `count(xs: u8[])`). An index, a length or a field reads the value in place, and so does a
  method of it and a function the value or a part of it is passed to, also the callbacks it
  passes them to (`s.items.some((x) => x.ok)`) and its loops over arrays and maps (the
  compiler builds a copy of the function that reads them without counting them). What keeps
  the value or a part of it gets a copy: a parameter that keeps its argument (an `async`
  function's, a stored one), a variable bound to a part (`const ps = s.ports`), a method's
  result that is a part of it (`s.get(k)` of a `shared<Map<string, Item>>`), and a call
  through a function value. Such a copy is another object: `s.get("k") === s.items[2]` and
  `s.items.find((x) => x.v == 2) === s.items[2]` are `false`, where JavaScript compares the
  shared object with itself. A change to a part is refused where the compiler follows how it
  got there: through a variable bound or assigned to it, a `for…of` loop over the value (or
  over what a method of it returns, such as `s.values()`), a conditional, what a method or
  getter of it returns (`s.m.get(k)!.v = 1`), a closure at any depth, a function changing it,
  and through a container holding it (`[s.items[0]]`, `xs.slice()`, an object's field; the
  container's own slots may change). `subarray()` and `buffer` of a shared typed array are
  refused too: in JavaScript all of them reach the shared value itself. Not yet followed (a
  change lands on a copy, #1405): a part returned by a function or closure (other than a method
  or getter of the value: generic, static and interface functions, another object's method), a
  spread or `Array.from` copy, a generator, an `await`, `any` or `Reflect`, and a container
  filled with `push` / `set`. Change a copy (`.clone()`, `s.ports.slice()`) instead.
  `console.log(s)`, `[...s]`, `Object.keys(s)` and `JSON.stringify(s)` read the value inside
  as well. A value of
  `T` that holds a class with subclasses or with a `clone()` of its own is not read like this
  (copying it would run that method on objects other threads read): `.clone()` the `shared`
  and pass it on as a `shared<T>`. While the value or a part of it is read while code runs (a
  call it is passed to, a loop over it, a method it is the receiver of, a later argument),
  giving the variable that holds the handle another one is refused where it would be for an
  array (the loop's body, a closure passed to the same call); where it is allowed (a closure
  kept in a variable, a module `let` another function reassigns, a field or element holding
  the handle), the read keeps the box it started with alive, as JavaScript keeps the old array.
  Changing the value is an error (``cannot modify a `shared<number[]>`: a shared value is
  read-only …``), and so is passing it or a part of it (`s.m.get(k)!`) to a function that may
  change it, also one that keeps its argument (an `async` function, a generator, a parameter
  it stores) or changes what it returns (`st.m.get(k)!.v = 1`), or to a function value, and
  calling a method of it that changes it (also one returning the value it changes, such as
  `fill` or a method ending in `return this`): share a `Mutex` instead.

  ```ts
  async function count(xs: shared<u8[]>, b: u8): Promise<i64> {
    return countIn(xs, b);
  }

  function countIn(xs: u8[], b: u8): i64 {
    let n: i64 = 0;
    for (let i: usize = 0; i < xs.length; i++) {
      if (xs[i] == b) {
        n++;
      }
    }
    return n;
  }

  async function main() {
    const data = shared([1 as u8, 2, 1, 3, 1]);
    const ones = spawn(count(data, 1));
    const threes = spawn(count(data, 3));
    console.log(await ones, await threes, data.length, data[0]);
  }
  ```
- `shared(new Mutex<T>(x))` with `m.with((v) => …)`: a synchronous lock. The callback gets the
  value itself (assigning `v` updates it), returns a result, and must not be async. Its value
  is read only there: a value holding a `Mutex` cannot be printed, written as JSON or
  converted to `any`, `unknown` or `object` (``cannot convert a `shared<Mutex<C>>` to `any`:
  it holds a lock``), all of which would read it without the lock, also not through a type
  parameter (a generic function printing its `T` is refused where it is called with one). Nothing
  crosses the lock by reference, since other threads use the value as soon as it is released:
  - The result leaves the lock like a value going to another task: an object the callback made
    moves out, a part of the protected value comes out as a copy (`m.with((v) => v.inner)` is
    a snapshot; change the value inside the callback). A part that owns a resource without
    `clone()` cannot be copied, so returning one is an error.
  - So does a part of the value the callback stores into something it captured
    (`out.push(v.inner)` pushes a copy, `last = v.inner` assigns one), and an outside object it
    stores into the value (`v.items.push(item)` stores a copy; `item` stays outside, so using
    it after the `with` is an error, "`item` is still used after `with` stored it in the
    locked value": store `item.clone()` to keep using `item`) — also when a function or
    method the callback calls does the storing (`v.giveTo(out)` gives the method a copy of the
    value). A resource without `clone()` cannot be copied, so the value gets the object itself:
    using the variable afterwards is an error ("`conn` is still used after `with` stored it in
    the locked value"), and so is storing one from a field or an element, which keeps it too
    (give the type a `clone()`, or move it into the value). An object
    stored from one place in the value to another, or from one outside object to another, stays
    the same object. A call that stores a part of an argument it also changes cannot be given a
    copy, and is an error ("this call may store a part of the locked value outside it, and also
    changes that argument"): return the part from `with` instead. A function value the
    callback calls with the value (`step(v)`, a helper's parameter) gets the value itself, so
    its changes land in it: the closures it may be are checked like the callback. One whose
    body cannot be found (a field, an array element) may not be given both the value and
    something outside the lock. The callback itself is a closure written where it is passed, a
    named function (`m.with(update)`), or a variable or helper parameter bound to those, and
    is checked as above. One found only through a field, an array element, a `Map` value or a
    generic factory (`this.m.with(this.reducer)`, `for (const op of ops) m.with(op)`) may be
    any closure or function taking the value's type (a generic one through each of its
    instantiations), except the closures written as `with`'s argument: each of them is
    checked, without being changed. When the value is an object value and one of them would
    store across the lock or make a promise from the value, the call is an error naming that
    function ("the function passed to `with` comes from an object's field, and may be a
    closure that stores a part of the locked `State` outside it …"); otherwise the call is
    accepted as written.
  - A promise made from the value runs after the lock is released. One that only reads what it
    is given gets a copy, like a spawned call (`m.with((v) => save(v.name))`,
    `m.with((v) => read(v))`: `read` sees the value as it was; a copy of a resource without
    `clone()` cannot be made, which is an error naming it). The copy is a deep copy of what
    the promise is given, made while the lock is held — the whole value for `read(v)` — so
    pass only the parts the work needs (`read(v.config)`) when the value is large; strings are
    never copied. Storing a part outside costs a copy of that part the same way. A promise
    that changes what it is given is an error, whether the callback returns, stores or drops
    it, or a function it calls starts it (`m.with((v) => bump(v))`: "this `Promise<…>` uses
    the locked value, and would run after
    `with` releases the lock"): take what the work needs out of the value, await outside
    `with`, and store the result with another `with`. A function value whose body is not
    visible may not return a promise either; through a generic helper
    (`function run<T>(m, fs: ((s: S) => T)[]): T`) that is reported where `T` is a promise.
  - A function value stored in the value must not have captured a resource without `clone()`
    (the program stops when the lock is released). Calling one that returns or starts a
    promise (`m.with((f) => f())`) is an error when a synchronous closure of its type captured
    objects: the promise would use them after the lock is released. An async closure there is
    fine, since each call gets its own copy of what it captured.
  - The callback cannot use module state (module-level `let`s and kept constants), directly
    or through a function it calls, a field initializer, or a `toString()`, `toJSON()` or
    `[Symbol.dispose]()` method that std or a drop calls ([Module state](variables.md#module-state)).

`shared(x)` is a thread boundary like `spawn`, and it takes `x` itself: a variable used after
it went into `shared(...)` (also inside `new Mutex(o)`, a literal or a constructor call there)
is an error ("`o` is still used after `shared(...)`; `shared` takes the value itself …"):
use it through the `shared` value from then on, or pass `o.clone()`. A reference the compiler
cannot see (the object is also in an array, say) gets a copy at run time, and a resource
without `clone()` then stops the program. A function value in it, and an HTTP handler, may be
called from several threads at once, and each call gets its own copy of what the function
captured, so a captured resource needs a `clone()`: capturing one without it there is an
error ("this function captures `store`, and it handles HTTP requests, …"), or, when the
function arrives through a parameter, ``panic: a function value that captured a `Store` is
shared between threads …`` where the `shared` or the server is made. Capture a
`shared(new Mutex(store))` instead to use one resource from every call.

## Errors

Errors are typed like everywhere else ([Errors](errors.md)). A promise's type carries what it
can reject with: `Promise<T, E>` (a `Promise<T>` never rejects).

- An async function's `throws` clause (`async function f(): Promise<T> throws E`, or the
  inferred one) is its promise's `E`. In a function type, `throws` after a `Promise` result is
  the promise's `E`: `() => Promise<T> throws E`. So it is for an interface method returning a
  promise (`load(id: string): Promise<User> throws NotFound`, or `Promise<User, NotFound>`):
  calling it through the interface returns a `Promise<User, NotFound>`, and its
  implementations are `async` methods (a synchronous one may return a promise only when the
  method's promise cannot reject). The same holds for a class method returning a promise that
  a subclass overrides: the base method and every override are `async` when any of them can
  fail. A getter cannot be `async`, so a getter returning a promise from an interface cannot
  fail, and overridden getters throw at the read. A method declared as returning a type
  parameter (`get(): T`) throws its errors for every type argument: an `async` implementation
  for `T = Promise<…>` must not fail. A default body of such an interface method can be `async` too:

```ts
class NotFound extends Error {}

interface Store {
  get(id: string): string | null;
  async load(id: string): Promise<string> throws NotFound {
    const v = this.get(id);
    if (v == null) {
      throw new NotFound(id);
    }
    return v;
  }
}

class Memory implements Store {
  get(id: string): string | null {
    return id == "a" ? "apple" : null;
  }
}

async function main() {
  const s: Store = new Memory();
  console.log(await s.load("a"));
  try {
    await s.load("b");
  } catch (e) {
    console.log("not found:", e.message);
  }
}
```
- Awaiting rethrows the typed error: a direct `await f()`, a stored promise
  (`const p = f(); … await p`), a spawned task's handle, `await Promise.race(ps)` (the first
  promise to settle), and `await Promise.all(ps)`, which rejects as soon as one promise rejects,
  like JS (the others keep running to completion). `Promise.allSettled` reports each rejection as
  `{ status: "rejected"; reason: E }`.
- A promise converts to a promise type whose error type allows all of its errors: a
  `Promise<T>` can be used as a `Promise<T, E>`, and a `Promise<T, E1>` as a
  `Promise<T, E1 | E2>`, wherever that type is expected (a typed variable or array, an
  argument, a return value). An array literal without an expected type still takes its element
  type from its first element, so mixed arrays need a type:
  `const ps: Promise<string, Timeout>[] = [work(), rejectAfter(50)]`.
- A promise nobody can await reports its error as uncaught (`Uncaught <Type>: <message>`, exit
  code 1), like an unhandled rejection: a task spawned as a statement (`spawn(f());`, or
  `spawn(p);` of a stored promise), a spawned task whose handle was dropped without being
  awaited (when the task rejects, or when the handle is dropped after it did), and a stored
  promise that rejects after it was dropped unawaited. As in Node, a stored promise that
  rejects while its owner does not wait for it is reported too, once the callbacks queued
  before have run (the microtask queue is drained), even though the owner still holds it:
  `const p = f(); await sleep(10); await p;` ends the process at the rejection if `f` rejects
  first. Awaiting it (or chaining `catch`, or handing it to a combinator) before then handles
  it. A promise or handle handed to a
  combinator (`Promise.race`, `all`, `any`) is handled, and so is a `scope.spawn` child, whose
  error fails its `taskScope`.

## `new Promise`

`new Promise<T, E>((resolve, reject) => …)` runs the executor at once and wraps callback APIs.
`T` and `E` come from the type arguments or from the expected type (`Promise<T>` cannot reject;
`new Promise((resolve) => …)` takes no `reject`).

- `resolve` and `reject` are owned handles to the promise's settle-once slot on the heap, not
  closures from a caller: the executor may store them, capture them in callbacks, pass them to
  `spawn` or `setTimeout`, and call them later from any task.
- The first `resolve(value)` or `reject(reason)` settles the promise; later calls do nothing.
  An error the executor throws rejects it.
- For a `Promise<void>`, `resolve()` takes no argument (a trailing `void` parameter may be
  left out, as in TypeScript): `await new Promise<void>((resolve) => resolve())`.
- A value settled on the promise's own task is the same object the awaiter gets (like JS); one
  settled from another task is transferred like a `spawn` argument, once the settling task has
  finished its current step: moved when that task no longer references it (a value made for the
  call, `resolve(new Result(…))`), copied when it still does. Such a settlement lands when
  the settling task next yields (an `await` that waits, or its end), so the awaiter wakes then. A
  value that cannot reach an object (a number, a string, a struct of them) has nothing to
  transfer and settles at once. (On single-threaded WebAssembly
  there is only one thread, so it is shared there too.) A promise passed on to a spawned task
  and awaited there delivers a copy when the settling task still uses the value.
- The executor must be an arrow-function literal (``the executor of `new Promise` must be an
  arrow function``); it runs at once and is released before the promise waits.
- A promise whose `resolve` and `reject` are all dropped without settling never settles, like
  in JS (it can lose a `Promise.race`). Awaiting it directly (`await new Promise(…)`) is
  reported instead of waiting forever: ``panic: awaiting a promise whose resolve and reject were
  dropped without settling (created at file:line:col)``, exit code 101.

```ts
import { setTimeout } from "velt:timers";

class Failed extends Error {}

function after(ms: i64, ok: bool): Promise<string, Failed> {
  return new Promise((resolve, reject) => {
    setTimeout(async () => {
      if (ok) {
        resolve(`fine after ${ms} ms`);
      } else {
        reject(new Failed("not fine"));
      }
    }, ms);
  });
}

async function main() {
  console.log(await after(10, true)); // fine after 10 ms
}
```

## `Promise.withResolvers`

`Promise.withResolvers<T, E>()` (ES2024) returns a `PromiseWithResolvers<T, E>`:
`{ promise: Promise<T, E>; resolve: (value: T) => void; reject: (reason: E) => void }`, a
pending promise and the functions that settle it, without an executor. `T` and `E` come from
the type arguments (`E` defaults to `never`: the promise cannot reject) or from the expected
type.

- `resolve` and `reject` work like a `new Promise` executor's: store them, move them into a
  spawned task, or send them over a [channel](../std/channel.md) and call them there; the
  awaiting task wakes. The first settlement wins and later calls do nothing.
- A value settled from another task is transferred (moved, or copied while that task still uses
  it), one settled on the promise's own task is the same object, as for `new Promise`.
- A promise whose `resolve` and `reject` are all dropped without settling never settles, like in
  JS: it can lose a `Promise.race`, and awaiting it otherwise waits forever. A pending promise
  keeps the process alive (#147) after `main` returns, but not after it fails (an uncaught
  error or a nonzero exit code ends the process at once, like an uncaught exception in Node).

A one-shot reply to a request handled on another task, without a channel per request:

```ts
import { channel, Channel } from "velt:channel";

type Request = { n: i64; reply: (value: i64) => void };

async function doubler(requests: Channel<Request>) {
  while (true) {
    const r = await requests.receive();
    if (r == null) {
      return;
    }
    r.reply(r.n * 2);
  }
}

async function main() {
  const requests = channel<Request>();
  const server = spawn(doubler(requests));
  const { promise, resolve } = Promise.withResolvers<i64>();
  await requests.send({ n: 21, reply: resolve });
  console.log(await promise); // 42
  requests.close();
  await server;
}
```

**Planned** ([semantics — promises](../internals/design/semantics.md#promises)): a stored
promise may borrow its arguments instead of owning them when it provably finishes before they
change.

## Example

```ts
async function delayed(ms: i64, v: i64): Promise<i64> {
  await sleep(ms);
  return v;
}

async function worker(id: i64, hits: shared<i64>): Promise<i64> {
  hits.add(1);
  return id;
}

async function main() {
  const slow = delayed(30, 1);                                           // starts now
  const fast = delayed(10, 2);                                           // runs concurrently
  console.log(await fast, await slow);                                   // 2 1, after ~30 ms
  const [a, b] = await Promise.all([delayed(20, 1), delayed(10, 2)]);   // concurrent
  const first = await Promise.race([delayed(50, 7), delayed(5, 8)]);    // 8
  const hits = shared(0);
  const tasks: Promise<i64>[] = [];
  for (let i = 0; i < 4; i++) {
    tasks.push(spawn(worker(i, hits)));                                  // on any core
  }
  const ids = await Promise.all(tasks);
  const log = shared(new Mutex<string[]>([]));
  log.with((v) => {
    v.push(`sum ${a + b + first}`);
  });
  console.log(ids.length, hits.get(), log.with((v) => v.length));
}
```

```ts error
async function save(key: string): Promise<void> {
  console.log("saved", key);
}

async function main() {
  save("a");       // error: floating promise — `await save("a")` or `spawn(save("a"))`
}
```
