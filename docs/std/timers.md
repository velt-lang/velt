# velt:timers

`import { setTimeout, Ticker } from "velt:timers"`. One-shot timers and a pull-based interval.
A timer takes a callback returning the promise to run (`() => save(doc)` or `async () => { … }`),
like JS. The callback owns what it captures; the task runs as its own spawned task, so an error
it throws is uncaught (catch inside the task).

- `setTimeout(task: () => Promise<void>, ms): Timer`, `setImmediate(task): Timer`,
  `clearTimeout(t)`, `delay(ms)` (the same as `sleep`).
- `Timer`: `clear()` cancels the task if it hasn't started; `cleared`, `started`. Dropping a
  `Timer` does not cancel it.
- `new Ticker(periodMs)`: a drift-free schedule; missed ticks are skipped, not burst.
  - `tick(): Promise<bool>`: resolves false once the ticker is stopped.
  - `stop()`, `stopper(): TickerStop`: `TickerStop.stop()` works from another task. A stop
    wakes a pending `tick()` at once, which resolves to false; the sleep it was in is
    cancelled, so a long period never delays shutdown.
  - A `Ticker` is an `AsyncIterable<i64>`: `for await (const n of ticker)` waits for each tick
    like `tick()` and gets its number (1, 2, … counted per loop) until the ticker is stopped.
    Leaving the loop early does not stop the ticker.

```ts
import { setTimeout, Ticker } from "velt:timers";

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
