# velt:timers

`import { setImmediate, Ticker } from "velt:timers"`. Timers and a pull-based interval.
`setTimeout`, `clearTimeout`, `setInterval`, `clearInterval` and `Timer` are globals (from the
[prelude](prelude.md)), as in TypeScript; `velt:timers` exports them too. A timer takes a
callback, like JS: one returning the promise to run (`() => save(doc)` or `async () => { … }`),
or a plain one (`() => console.log("x")`, a function name or a local function value,
`setTimeout(tick, 10)`, whose parameters, if any, are optional or have defaults and are left
out), which the compiler runs as an `async` one. The callback owns what it captures; the task
runs as its own spawned task, so an error it throws is uncaught (catch inside the task).

- `setTimeout(task: () => Promise<void>, ms): Timer`, `setImmediate(task): Timer`,
  `clearTimeout(t)`, `delay(ms)` (the same as `sleep`).
- `setInterval(task: () => Promise<void>, ms): Timer` runs `task` every `ms` milliseconds (at
  least 1, like Node) until `clearInterval(t)`, on a fixed schedule like Node's: a run is due
  every `ms`, also while the previous run's promise is still pending. Runs that overlap
  interleave at their `await`s on the timer's task, like on Node's one thread; they never run
  in parallel.
- `Timer`: `clear()` cancels a timeout that hasn't started and stops an interval at any time;
  `cleared`, `started` (an interval: it ran at least once). `clearTimeout` and `clearInterval`
  both work on either kind, as in Node. Dropping a `Timer` does not cancel it. `unref()`,
  `ref()` and `hasRef()` work like Node's `Timeout` (`unref()` and `ref()` return the timer).

A pending timer keeps the process alive after `main` returns, as in Node: the program ends once
every timer has fired and its callback's promise has settled, or was cleared. An interval keeps
it alive until it is cleared (and its runs in progress have settled). A timer that is `unref()`ed
does not keep the process alive. `process.exit()` and an uncaught error end the process at once,
pending timers or not.

```ts
async function main() {
  const t = setInterval(() => console.log("tick"), 1000);
  t.unref(); // the interval runs only while something else keeps the process alive
  setTimeout(async () => console.log("done"), 10); // printed after main returns
}
```

What keeps a process alive is a *handle*, as in Node: a pending ref'd timer, a listening
[server](http.md). A spawned task (`spawn(p)`) is not a handle: the process does not wait for
it. A started promise that is not awaited runs to completion when its task finishes first (and
the process waits for it), see [Async](../reference/async.md#promises); in Node it would keep
the process alive only through the handles it waits on, so a promise waiting for something that
never comes (a `resolve` that is never called) ends a Node process but keeps a Velt one waiting.

Differences from Node:

- The handle is a `Timer`, not Node's `Timeout`: no `refresh()` or conversion to a number.
- The process waits for a timer's callback promise to settle. In Node only what the callback
  waits for (timers, sockets) keeps the process alive: the same for ordinary code (an
  `await sleep(ms)` in the callback keeps both alive), but a callback waiting for something
  that never comes keeps a Velt process waiting.
- Extra arguments after `ms` are not supported: capture them in the callback. A callback
  passed as a value other than an arrow, a function name or a local (`setTimeout(this.tick,
  10)`) must return a promise.

## Ticker

`new Ticker(periodMs)` is a drift-free schedule; missed ticks are skipped, not burst.

- `tick(): Promise<bool>`: resolves false once the ticker is stopped.
- `stop()`, `stopper(): TickerStop`: `TickerStop.stop()` works from another task. A stop wakes
  a pending `tick()` at once, which resolves to false; the sleep it was in is cancelled, so a
  long period never delays shutdown.
- A `Ticker` is an `AsyncIterable<i64>`: `for await (const n of ticker)` waits for each tick
  like `tick()` and gets its number (1, 2, … counted per loop) until the ticker is stopped.
  Leaving the loop early does not stop the ticker.

```ts
import { Ticker } from "velt:timers";

async function remind(msg: string): Promise<void> {
  console.log(msg);
}

async function main() {
  const t = setTimeout(() => remind("later"), 20);
  const cancelled = setTimeout(() => remind("never printed"), 10);
  cancelled.clear();
  const ticker = new Ticker(5);
  let ticks = 0;
  for await (const n of ticker) {
    ticks = n;
    if (n == 3) {
      ticker.stop();
    }
  }
  await sleep(40);
  console.log(ticks, t.started, cancelled.cleared); // 3 true true
}
```

A `Ticker` stops through an [`AbortController`](task.md): each `tick()` races its sleep against
the stop signal.
