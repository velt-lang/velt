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
- **A dropped promise is not cancelled**: a stored promise that is never awaited still runs to
  completion (its result is dropped), and the program waits for it before exiting, like Node
  waits for pending work. A promise created outside async code (for example in a synchronous
  `main`) starts when it is awaited or spawned.
- An async call owns its arguments: an argument variable used again afterwards is shared with
  the promise (objects) or copied (numbers, strings), otherwise moved, because the promise may
  outlive the caller's frame. A promise has one owner: using a promise variable after handing
  it on is ``use of moved value``, and an explicit `p.clone()` is ``a promise cannot be copied``.
- Values handed to `spawn` (and captured by an HTTP handler, sent over a channel, or settled on
  a promise from another task) go to another thread. What the program no longer references
  anywhere else moves as it is; an object it still shares is deep-copied for the task (like a
  structured clone), so threads never share reference counts. That includes the receiver of
  `spawn(obj.method())` (also through a base-class reference or an interface value) and what a
  closure or interface value passed to the task reaches. A closure the caller still uses
  afterwards is copied too, with what it captures (also a variable it assigns), so the task and
  the caller each run their own copy. A closure handed on for the last time that captures only
  values nothing else references (an HTTP handler capturing a disposable resource, say) goes to
  the task as it is. `spawn(async () => …)` and `spawn((async () => …)())` hand the captured
  values themselves to the task, so a captured resource the program no longer uses moves and
  its `[Symbol.dispose]()` runs once, on the task. Each call of an async closure otherwise gets
  its own copy of what the closure captured, except a resource without `clone()`, which the
  call shares with the closure (it is released once, after both). So `spawn(f())` through an
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
  is shared behind a [`Mutex`](#thread-safety)). When another reference is only found at run time
  (the value is also in an array, say), the program stops with ``panic: cannot copy a `Conn`
  for another task …``.

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

All promises in one call must have the same type. Like in JS, every promise passed to a
combinator is *handled*: one that loses (or is left behind) and rejects later has its error
dropped, not reported as uncaught, so a timeout written as a rejecting promise in a
`Promise.race` is fine once the work won (`Promise.allSettled` awaits every promise itself).
Losing promises that already started, such as calls of async functions, run to completion;
a runtime operation that loses, such as `sleep(ms)` or an I/O call, is cancelled. A combinator
kept as a value is itself a stored promise: if nobody awaits it, its own rejection is reported
as uncaught.

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

Thread safety is checked at compile time: async closures, and HTTP handlers, must not modify
captured variables; the error mentions "spawned task" and `shared`. Share state with:

- `shared(x)`, which gives a `shared<T>`: an atomically reference-counted value. Assigning,
  passing or capturing it adds a reference (so does `.clone()`); it is never deep-copied. For
  64-bit integers, `.add(n)`, `.get()` and `.set(v)` are atomic.
- `shared(new Mutex<T>(x))` with `m.with((v) => …)`: a synchronous lock. The callback gets the
  value itself (assigning `v` updates it), returns a result, and must not be async. The result
  leaves the lock like a value going to another task: an object the callback made moves out,
  a part of the protected value comes out as a copy (`m.with((v) => v.inner)` is a snapshot;
  change the value inside the callback). A function value stored in the value must not have
  captured a resource without `clone()` (the program stops when the lock is released).

`shared(x)` is a thread boundary like `spawn`: `x` is transferred (moved, or copied when the
program still references it elsewhere). A function value in it, and an HTTP handler, may be
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
  `spawn(p);` of a stored promise), and a stored promise that rejects after it was dropped
  unawaited (unless a combinator handled it).

## `new Promise`

`new Promise<T, E>((resolve, reject) => …)` runs the executor at once and wraps callback APIs.
`T` and `E` come from the type arguments or from the expected type (`Promise<T>` cannot reject;
`new Promise((resolve) => …)` takes no `reject`).

- `resolve` and `reject` are owned handles to the promise's settle-once slot on the heap, not
  closures from a caller: the executor may store them, capture them in callbacks, pass them to
  `spawn` or `setTimeout`, and call them later from any task.
- The first `resolve(value)` or `reject(reason)` settles the promise; later calls do nothing.
  An error the executor throws rejects it.
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
  keeps the process alive (#147).

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
