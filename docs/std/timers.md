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
  - `stop()`, `stopper(): TickerStop`: `TickerStop.stop()` works from another task.

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
  while (await ticker.tick()) {
    ticks++;
    if (ticks == 3) {
      ticker.stop();
    }
  }
  await sleep(40);
  console.log(ticks, t.started, cancelled.cleared); // 3 true true
}
```

Notes: a stop takes effect when the pending tick is due, at most one period later.
