# Async and concurrency

Velt's `async`/`await` behaves like JavaScript's: promises start when you create them, and
output order matches Node. Underneath, async functions compile to state machines running on a
multi-threaded tokio runtime, so a directly awaited call costs no allocation, and `spawn` puts
work on other cores. This guide covers the patterns; the exact rules are in
[the Reference](../reference/async.md).

## Promises start at once

```ts
async function fetchPrice(item: string): Promise<f64> {
  await sleep(item.length as i64 * 5);            // pretend to do I/O
  return item.length as f64 * 1.5;
}

async function main() {
  const tea = fetchPrice("tea");                  // starts now
  const coffee = fetchPrice("coffee");            // runs concurrently with tea
  console.log(await tea, await coffee);           // 4.5 9, after ~30 ms, not ~45

  const all = await Promise.all(["tea", "milk"].map((i) => fetchPrice(i)));
  console.log(all);                               // [ 4.5, 6 ]
}
```

- `await f()` runs `f` in place: no task, no allocation. Use it whenever you need the result
  right away.
- `const p = f()` starts `f` now; it runs until its first `await`, then concurrently with you.
  It costs one small allocation.
- `f();` on its own line is a compile error, "floating promise": either `await` it or
  `spawn` it. In JavaScript a forgotten `await` silently drops errors.
- A promise you never await still runs to completion, and the program waits for it before
  exiting, like Node.

Started promises run on their creator's task, one at a time, like JavaScript's single thread.
That is why they need no locks.

## Timeouts with `Promise.race`

`Promise.race` settles with the first promise to settle. Like in JavaScript, every promise in
the race counts as handled: the losers keep running to completion, and a loser that rejects
after the race is over is dropped, not reported as an uncaught error. So a timeout can simply
throw:

```ts
class OutOfStock extends Error {}
class TooSlow extends Error {}

async function fetchPrice(item: string): Promise<f64> throws OutOfStock {
  await sleep(item.length as i64 * 5);
  if (item == "caviar") {
    throw new OutOfStock(`no ${item} today`);
  }
  return item.length as f64 * 1.5;
}

async function priceWithin(item: string, ms: i64): Promise<f64> throws OutOfStock | TooSlow {
  const price = async (): Promise<f64> throws OutOfStock | TooSlow => await fetchPrice(item);
  const timeout = async (): Promise<f64> throws OutOfStock | TooSlow => {
    await sleep(ms);
    throw new TooSlow(`no price within ${ms} ms`);
  };
  return await Promise.race([price(), timeout()]);
}

async function main() {
  console.log(await priceWithin("water", 100));               // 7.5, and the timer is dropped
  try {
    await priceWithin("a very long name", 10);
  } catch (e) {
    console.log(e.message);                                   // no price within 10 ms
  }
}
```

Every promise in one `Promise.race` (or `all`, `allSettled`, `any`) must have the same type,
including the error type: `Promise<f64, OutOfStock | TooSlow>` here. That is why both closures
declare the same `throws` clause; a `throws` clause may allow more than the body throws.

## Errors in concurrent work

Errors are typed in async code too: a promise's type carries what it can reject with, and
`await` rethrows it.

```ts
class OutOfStock extends Error {}

async function fetchPrice(item: string): Promise<f64> throws OutOfStock {
  await sleep(5);
  if (item == "caviar") {
    throw new OutOfStock(`no ${item} today`);
  }
  return 4.5;
}

async function main() {
  try {
    await Promise.all([fetchPrice("tea"), fetchPrice("caviar")]);
  } catch (e) {                                   // e: OutOfStock
    console.log("all failed:", e.message);
  }

  const results = await Promise.allSettled([fetchPrice("tea"), fetchPrice("caviar")]);
  for (const r of results) {
    switch (r.status) {
      case "fulfilled":
        console.log("ok", r.value);               // ok 4.5
        break;
      case "rejected":
        console.log("failed:", r.reason.message); // failed: no caviar today
        break;
    }
  }
}
```

`Promise.all` rejects as soon as one promise rejects, like JavaScript. The other promises keep
running to completion; their results and any later rejections are dropped.
`Promise.allSettled` reports each result as a discriminated union. `Promise.any` gives the first
success, or an `AggregateError`.

## Using every core with `spawn`

`spawn(f())` or `spawn(async () => …)` runs work as its own task on any core and returns a
promise for the result. A spawned task runs even if nobody awaits it.

```ts
function sumOfSquares(from: i64, to: i64): i64 {
  let total = 0;
  for (let i = from; i < to; i++) {
    total += i * i % 7;
  }
  return total;
}

async function main() {
  const chunks: Promise<i64>[] = [];
  for (let c = 0; c < 4; c++) {
    chunks.push(spawn(async () => sumOfSquares(c * 1000000, (c + 1) * 1000000)));
  }
  const parts = await Promise.all(chunks);       // four cores at once
  console.log(parts.reduce((a, b) => a + b, 0)); // 7999999
}
```

## Sharing state between tasks

Spawned tasks run in parallel, so the compiler doesn't let them modify captured variables
("cannot mutate captured variable `n` in a spawned task"), and an object a task captures is
copied for it (a structured clone) when the rest of the program still uses it. Data races are
compile errors. Share state explicitly:

- `shared(x)` creates an atomically reference-counted value: tasks that capture it refer to the
  same value. On 64-bit integers, `add`, `get` and `set` are atomic.
- `shared(new Mutex<T>(x))` guards any value; `m.with((v) => …)` locks it for the callback, which
  gets the value itself and may return a result (a copy of anything that is part of the value).
  `shared` takes `x` itself: after `shared(new Mutex(o))`, use `o` only through the `shared`
  value (or pass `o.clone()` to keep your own).

```ts
async function main() {
  const seen = shared(new Mutex<string[]>([]));
  const done = shared(0);
  const workers: Promise<void>[] = [];
  for (let w = 0; w < 3; w++) {
    workers.push(spawn(async () => {
      seen.with((v) => {
        v.push(`worker ${w}`);
      });
      done.add(1);
    }));
  }
  await Promise.all(workers);
  console.log(done.get(), seen.with((v) => v.length));   // 3 3
}
```

A `with` callback is synchronous: keep it short and don't `await` inside it. What it hands
out — its result, or a part of the value it stores into a variable or array it captured — is a
copy, and a promise made from the value is a compile error (it would run after the lock is
released): take a copy out, await, and put the result back with another `with`.

## Streams with `for await`

Work that arrives over time — jobs on a [channel](../std/channel.md), lines of a file,
WebSocket messages, ticks — is consumed with `for await`, as in TypeScript. A channel passes
values between tasks and ends the loop once it is closed and drained:

```ts
import { channel, Channel, ChannelClosed } from "velt:channel";

async function worker(id: i64, jobs: Channel<string>, results: Channel<string>): Promise<void> throws ChannelClosed {
  for await (const job of jobs) {
    await results.send(`worker ${id} did ${job}`);
  }
}

async function main() {
  const jobs = channel<string>(8);
  const results = channel<string>();
  const workers = [spawn(worker(1, jobs, results)), spawn(worker(2, jobs, results))];
  for (const job of ["resize", "upload", "notify"]) {
    await jobs.send(job);
  }
  jobs.close();                    // the workers' loops end once the queue is drained
  await Promise.all(workers);
  results.close();
  let n = 0;
  for await (const line of results) {
    n++;
  }
  console.log(n);                  // 3
}
```

Leaving a loop early (`break`) does not close the channel: the values still queued stay there
for other receivers. Your own sources are `async function*` generators
([Async generators](../reference/functions.md#async-generators)); the
[async iteration](../reference/async.md#async-iteration) reference lists the std ones.

## Timers

`sleep(ms)` pauses the current async function. [`velt:timers`](../std/timers.md) has
`setTimeout` (with `clear()`), `setImmediate` and `Ticker`, a drift-free interval you iterate
with `for await (const n of ticker)` or pull with `await ticker.tick()`. There is no global
`setTimeout`.

## Performance

From `bench/async` ([bench/RESULTS.md](../../bench/RESULTS.md#async); release builds on an
Intel i9-12900HK laptop under Windows 11, best of 5, wall-clock including process start, while
other builds were running, so ±15–20%):

| Benchmark | Velt | Rust tokio (multi-thread) | Node |
|---|---:|---:|---:|
| 10M sequential awaits | 55 ms | 52 ms | 508 ms |
| 500k awaits 21 frames deep | 379 ms | 433 ms | 743 ms |
| 1M spawned tasks | 614 ms | 703 ms | 1618 ms |
| 100k concurrent timers | 70 ms | 64 ms | 229 ms |

An idle Velt process uses about 2 MB more memory than the Rust one and about 50 MB less than
Node.
