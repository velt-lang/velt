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
- An async call owns its arguments: an argument variable used again afterwards is copied,
  otherwise moved, because the promise may outlive the caller's frame. Promises, and values
  holding one or a `[Symbol.dispose]` resource, cannot be copied: they are always moved
  (``a promise cannot be copied`` for an explicit `p.clone()`).

## Combinators

- `Promise.all(ps: Promise<T, E>[]): Promise<T[], E>`: the results, in order.
- `Promise.race(ps): Promise<T, E>`: the first to settle; the others keep running to
  completion.
- `Promise.allSettled(ps)`: a `PromiseSettledResult<T, E>[]`, where each element is
  `{ status: "fulfilled"; value: T } | { status: "rejected"; reason: E }`.
- `Promise.any(ps): Promise<T>`: the first to fulfill; `AggregateError` when all of them reject
  (or the array is empty).

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
`f` as a task of its own on any core, and a spawned task runs even if nobody awaits it. A
promise that already started stays on the task that started it.

Built-ins: `sleep(ms)`, `yieldNow()`, `performance.now(): f64` (monotonic milliseconds) and
`Date.now(): i64`. Timers and intervals are in [`velt:timers`](../std/timers.md).

## Thread safety

Thread safety is checked at compile time: async closures, and HTTP handlers, must not modify
captured variables; the error mentions "spawned task" and `shared`. Share state with:

- `shared(x)`, which gives a `shared<T>`: an atomically reference-counted value; `.clone()`
  adds a reference. For 64-bit integers, `.add(n)`, `.get()` and `.set(v)` are atomic.
- `shared(new Mutex<T>(x))` with `m.with((v) => …)`: a synchronous lock. The callback gets the
  value itself (assigning `v` updates it), returns a result, and must not be async.

## Errors

Errors are typed like everywhere else ([Errors](errors.md)). A promise's type carries what it
can reject with: `Promise<T, E>` (a `Promise<T>` never rejects).

- An async function's `throws` clause (`async function f(): Promise<T> throws E`, or the
  inferred one) is its promise's `E`. In a function type, `throws` after a `Promise` result is
  the promise's `E`: `() => Promise<T> throws E`.
- Awaiting rethrows the typed error: a direct `await f()`, a stored promise
  (`const p = f(); … await p`), a spawned task's handle, `await Promise.race(ps)` (the first
  promise to settle), and `await Promise.all(ps)`, which waits for every promise and then
  rethrows the first rejection in array order (unlike JS, which rejects as soon as one promise
  rejects). `Promise.allSettled` reports each rejection as
  `{ status: "rejected"; reason: E }`.
- A promise nobody can await reports its error as uncaught (`Uncaught <Type>: <message>`, exit
  code 1), like an unhandled rejection: a task spawned as a statement (`spawn(f());`), and a
  stored promise that rejects after it was dropped unawaited (unless a combinator handled it).

**Planned** ([semantics — promises](../internals/design/semantics.md#promises)): a stored
promise may borrow its arguments instead of owning them when it provably finishes before they
change; `new Promise((resolve, reject) => …)` for wrapping callback APIs.

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
    tasks.push(spawn(worker(i, hits.clone())));                          // on any core
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
