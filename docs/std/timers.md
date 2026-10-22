# velt:timers

`import { Ticker } from "velt:timers"`. Timers and a pull-based interval.
`setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`, `setImmediate`, `clearImmediate`
and `Timer` are globals (from the [prelude](prelude.md)), as in Node; `velt:timers` exports them
too. A timer takes a
callback, like JS: one returning the promise to run (`() => save(doc)` or `async () => { … }`),
or a plain one (`() => console.log("x")`, a function name or a local function value,
`setTimeout(tick, 10)`, whose parameters, if any, are optional or have defaults and are left
out), which the compiler runs as an `async` one. The callback runs on the task that created the
timer, never at the same time as that task's other code, so it shares what it captures with the
code around it, as in Node: what it changes is seen afterwards, and what the creating code
changes after `setTimeout` (`xs.push(2)`, `s = "changed"`) is seen by the callback. An error it
throws is uncaught, as in Node: it is reported (`Uncaught RangeError: …`) and ends the process
with exit code 1 at the throw, before the microtasks and timers queued after it run (catch inside
the callback). The same holds for an error a `queueMicrotask(f)` callback throws. An `async`
callback's rejection ends the process at once too, where Node first empties the microtask queue
(an unhandled rejection).

- `setTimeout(task: () => Promise<void>, ms): Timer` (`ms` at least 1, like Node:
  `setTimeout(f, 0)` is `setTimeout(f, 1)`), `clearTimeout(t)`, `delay(ms)` (the same as
  `sleep`).
- `setImmediate(task): Timer` runs `task` once the running code and the callbacks it queued are
  done and the timers due by the current pass over the timers have run (Node's check phase),
  before the timers created after it; `clearImmediate(t)` cancels it.
- `setInterval(task: () => Promise<void>, ms): Timer` runs `task` every `ms` milliseconds (at
  least 1, like Node) until `clearInterval(t)`, on a fixed schedule like Node's: a run is due
  every `ms`, also while the previous run's promise is still pending. Runs that overlap
  interleave at their `await`s on the timer's task, like on Node's one thread; they never run
  in parallel.
- `Timer`: `clear()` cancels a timeout that hasn't started and stops an interval at any time;
  `cleared`, `started` (an interval: it ran at least once). `clearTimeout` and `clearInterval`
  both work on either kind, as in Node. Dropping a `Timer` does not cancel it. `unref()`,
  `ref()` and `hasRef()` work like Node's `Timeout` (`unref()` and `ref()` return the timer).

Timer callbacks run in Node's order. Node keeps one list of timers per delay, in the order they
were created, and runs the lists by their expiry: the clock read when the list's first timer was
created, in whole milliseconds, plus the delay (lists due at the same time in the order they were
made). Each pass over the due timers reads the clock once and runs, list by list, every timer
due by then, so a timer joins the list of its delay, even when busy code ran before it was
created: `setTimeout(a, 10); setTimeout(b, 12); busy(5); setTimeout(c, 10)` runs `a`, `c`, `b`,
and `setTimeout(a, 5); busy(20); setTimeout(b, 4)` runs `a`, then `b`. Timers created in the same
millisecond count from the same moment (`setTimeout(a, 5); setTimeout(b, 4)` runs `b` first). A
timer created by a callback with the delay of a list being run waits for the next pass, after the
other lists due by then. An interval's next run is due its delay after the clock read just before
its callback ran, and joins its list after the timers its callback created. `clearTimeout` takes
the timer out of its list at once. Each task keeps lists of its own (so the timers of one task
follow Node's rule exactly), and the lists of all tasks run in one order by expiry. No callback runs before the code that created the timer
(a synchronous `main` too) and the callbacks it queued are done. Each callback runs up to its
first `await` and the callbacks (microtasks) it queued run before the next timer's callback
starts, as in Node, so a callback can still `clearTimeout` a timer due in the same tick. A timer
whose task is idle but slow to run it (a loaded machine) holds back the timers due after it
instead of being overtaken; one whose task is busy (a spawned task running code that does not
pause) does not hold back the timers of other tasks. Callbacks that `await` go on concurrently,
like in Node.

A pending timer keeps the process alive after `main` returns, as in Node: a timeout until its
callback starts, an interval until it is cleared. A timer that is `unref()`ed does not keep the
process alive, and once nothing else is left its callback does not run, also when it falls due
while the process is ending or while the last ref'd callback is still running past its due time.
`process.exit()` and an uncaught error end the process at once, pending timers or
not.

```ts
async function main() {
  const t = setInterval(() => console.log("tick"), 1000);
  t.unref(); // the interval runs only while something else keeps the process alive
  setTimeout(async () => console.log("done"), 10); // printed after main returns
}
```

What keeps a process alive is what can still happen, as in Node: a pending ref'd timer, a
listening [server](http.md), and code that runs or waits for a timer or I/O (a spawned task, a
started promise, a timer's callback once it started, `await sleep(ms)`, a read). A promise that
waits only for something that never comes (a `resolve` nobody calls) does not keep it alive: once
nothing else is left, the process ends, as Node's does ([Async](../reference/async.md#promises)).

Differences from Node:

- The handle is a `Timer`, not Node's `Timeout`: no `refresh()` or conversion to a number.
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
