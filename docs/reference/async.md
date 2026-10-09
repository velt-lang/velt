# Async and concurrency

## Async functions

`async function f(): Promise<T>` compiles to a state machine. `await` is allowed only inside
`async` functions and arrows (``` `await` is only allowed inside async functions ```). Async
methods and async arrows work the same way. `async function main()` runs on the runtime, a
multi-threaded tokio executor with one worker per core.

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
  it. When it finishes, whoever awaits it resumes at once, like a JS microtask, so output order
  matches Node. Only `spawn` puts work on another core.
- Timers of one task fire in order: by deadline, then in the order the `sleep` calls were made.
  So the started promises (and the task itself) waiting for timers that are due together resume
  in the order their timers were created, like `setTimeout` callbacks in Node. A timer that is
  already due when it is first waited for (`sleep(0)`, or a `sleep` awaited after its delay
  passed) also waits for its turn: the code after it never runs before the synchronous code
  that started it, nor before timers due earlier. Timers of different tasks have no order
  between them, since the tasks run in parallel.
- **A dropped promise is not cancelled**: a stored promise that is never awaited still runs to
  completion (its result is dropped), and the program waits for it before exiting, like Node
  waits for pending work. A promise created outside async code (for example in a synchronous
  `main`) starts when it is awaited or spawned.
- After `main` returns, the process also waits for *handles*, as in Node: a listening
  [server](../std/http.md) and a pending [timer](../std/timers.md) (`setTimeout`,
  `setInterval`) unless it is `unref()`ed. A spawned task is not a handle: tasks still running
  when nothing else keeps the process alive end with it. `process.exit()` and an uncaught error
  end the process at once.
- An async call owns its arguments: an argument variable used again afterwards is shared with
  the promise (objects) or copied (numbers, strings), otherwise moved, because the promise may
  outlive the caller's frame. A promise has one owner: using a promise variable after handing
  it on is ``use of moved value``, and an explicit `p.clone()` is ``a promise cannot be copied``.
  In a collection, a promise is replaced in place (`arr[i] = p`) and taken out with `pop()` or
  `splice(i, 1)`, or awaited with the others by `Promise.all(arr)`. TypeScript lets several
  places hold the same promise; Velt doesn't, so reading a promise element (`arr[i]`) and the
  methods that copy values out (`m.get(k)`, `m.values()`, `arr.at(i)`, `arr.slice()`, …) are
  compile-time errors for promises and for values that copy one (a struct with a promise
  field); a class instance holding a promise is shared, so reading it out works. Shared
  promises, which would make these reads work as in TypeScript, are planned (#212).
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

All promises in one call must have the same type. Like in JS, every promise passed to a
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
- `shared(new Mutex<T>(x))` with `m.with((v) => …)`: a synchronous lock. The callback gets the
  value itself (assigning `v` updates it), returns a result, and must not be async. Nothing
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
  promise that rejects after it was dropped unawaited. A promise or handle handed to a
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
