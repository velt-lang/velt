# velt:task

`import { AbortError, TimeoutError, timeout, taskScope } from "velt:task"`. Cancellation with
TypeScript's `AbortController` / `AbortSignal` (global, as in Node; this module exports them
too), timeouts, task scopes (structured concurrency) and task placement. [`fetch`](fetch.md) takes a signal.

Cancellation is cooperative. `abort()` marks the signal and wakes the tasks waiting for it;
code that takes a signal checks it with `throwIfAborted()`, or races its work against
`whenAborted()`. Nothing is dropped behind the program's back, so `finally` blocks and `using`
disposal run as usual.

- `new AbortController()`:
  - `signal: AbortSignal` returns its signal (the same object every time, like JS). Pass it
    anywhere, `spawn`ed tasks included.
  - `abort(reason = "This operation was aborted")` aborts it. Aborting twice does nothing.
- `AbortSignal`:
  - `aborted: bool` and `reason: string` (`""` while not aborted).
  - `throwIfAborted()` throws if aborted: a `TimeoutError` if an `AbortSignal.timeout` aborted
    it, otherwise an `AbortError` with the reason as its message. `TimeoutError` extends
    `AbortError`, so `throws AbortError` covers both.
  - `whenAborted(): Promise<void>` resolves once aborted. It never resolves if the signal is
    never aborted. Race it directly against work, `Promise.race([work(), signal.whenAborted()])`:
    when the work wins, the losing wait is dropped. An async function wrapped around
    `whenAborted()` would instead keep waiting, and keep the program running, until the signal
    is aborted.
  - `AbortSignal.timeout(ms)` makes a signal that aborts by itself after `ms` milliseconds
    (reason `"timed out after <ms> ms"`). Its timer goes away with the signal: one made per
    request and dropped when the request is done costs nothing afterwards, however long `ms`.
  - `AbortSignal.any(signals)` makes a signal that aborts as soon as any of `signals` does, with
    its reason. It keeps `signals` alive until then.
- `timeout(p, ms): Promise<T> throws E | TimeoutError` returns `p`'s result, unless `p` doesn't
  settle within `ms` milliseconds; then it throws `TimeoutError` (with `ms`). When `p` wins, the
  timer is cancelled. After a timeout, `p` is abandoned like a `Promise.race` loser: a runtime
  wait (`signal.whenAborted()`, a timer) is dropped, and a running async call keeps running,
  like JS, with its rejection handled. To stop it, give it a signal.
- Signals can't be made from numbers: only `new AbortController()`, `AbortSignal.timeout` and
  `AbortSignal.any` create them.

```ts
import { AbortController, AbortSignal, AbortError, TimeoutError, timeout } from "velt:task";

async function download(name: string, signal: AbortSignal): Promise<string> throws AbortError {
  for (let chunk = 0; chunk < 10; chunk++) {
    signal.throwIfAborted();           // stop between chunks
    await sleep(5);
  }
  return `${name}: 10 chunks`;
}

async function main() {
  const ac = new AbortController();
  const job = download("big.iso", ac.signal);
  await sleep(12);
  ac.abort("user pressed cancel");
  try {
    console.log(await job);
  } catch (e) {
    console.log(e.message);            // user pressed cancel
  }

  try {
    console.log(await timeout(download("small.txt", AbortSignal.timeout(1000)), 20));
  } catch (e) {
    console.log(e.message);            // timed out after 20 ms
  }
}
```

Differences from JavaScript:
- Errors are typed, so `throwIfAborted()` throws an `AbortError` (or `TimeoutError`) carrying
  the reason; JS throws the reason value itself.
- `reason` is a `string`.
- There is no `addEventListener("abort", …)`: the runtime keeps no callbacks, because they would
  break `velt dev` hot reload. Use `await signal.whenAborted()`, for example in a
  `Promise.race`.

## Task scopes

`taskScope<T, E>(body: (scope: TaskScope<E>) => Promise<T> throws E, options?)` runs `body`
with a `TaskScope` and settles only once the body and every task it started with `scope.spawn`
have finished, so no child outlives the scope. `E` is the error type of the whole scope: the
body's, and every child's.

- `scope.spawn(p: Promise<T, E>): Promise<T, E>` adds `p` to the scope as a child task and
  returns its handle; the scope waits for it. `p` is a promise value, so a call such as
  `scope.spawn(work(signal))` starts on the scope's own task, like any stored promise, and the
  child task waits for it: CPU-bound work doesn't move to another core (use `spawn` for that).
  A promise that can't fail, or fails with part of `E`, is accepted too (it converts to
  `Promise<T, E>`).
- The scope returns the body's result, or fails with the **first error in time**: the body's
  or any child's, even a child nobody awaited or whose error the body caught. Errors that
  come later, such as the `AbortError`s that the cancellation causes in siblings, don't replace
  it.
- `scope.signal` is aborted when the body or a child fails (unless
  `options.cancelOnError` is `false`), on `scope.cancel(reason)`, and when the runtime drops the
  scope's own task. Pass it to the children so they stop cooperatively.
- `scope.spawn` after the scope ended is an error (it panics).

```ts
import { AbortSignal, AbortError, TaskScope, taskScope } from "velt:task";

async function fetchPart(n: i64, signal: AbortSignal): Promise<i64> throws AbortError {
  for (let i = 0; i < n; i++) {
    signal.throwIfAborted();
    await sleep(2);
  }
  return n * 10;
}

async function main() {
  const total = await taskScope(
    async (scope: TaskScope<AbortError>): Promise<i64> throws AbortError => {
      const signal = scope.signal;
      const a = scope.spawn(fetchPart(3, signal));
      const b = scope.spawn(fetchPart(5, signal));
      return (await a) + (await b);
    },
  );
  console.log(total); // 80
}
```

## Task placement

`workerCount()` and `currentWorker()`, with `spawn(p, { worker })` (a built-in), place tasks on
workers: the threads that run tasks (`VELT_THREADS`, else the number of cores; 1 on
WebAssembly), plus the spare ones started while tasks compute without pausing (below). A sharded program (one task per worker owning a slice of the data,
Seastar or Orleans style) pins each shard to a worker and its callers next to it, so a call
never wakes another thread.

- `workerCount(): i64`: how many workers there are.
- `currentWorker(): i64`: the worker running this task (`0` to `workerCount() - 1`); `0`
  outside a task (a synchronous `main`).
- `spawn(p, { worker: i })`: starts `p` on worker `i`, where it stays (`SpawnOptions` is
  `{ worker?: i64 }`; `{}` pins nothing). `p` must be a call or a task body: a promise that
  already started stays on its task (`a started promise stays on its task; pass the call`).
  The options must be an object literal (a variable holding them is an error); `worker` may
  be given once. An index out of range is a panic:
  `spawn: worker 7 is out of range (the runtime has 4 workers)`.
- Tasks run on the runtime's own scheduler: every worker runs its own event loop, a task stays
  on the worker it started on (an unpinned one starts on the spawning worker), and another
  worker takes on part of a worker's tasks only while that one stays busy and the help raises
  how much gets done. A pinned task never moves. A task that computes without pausing for more
  than a few milliseconds doesn't hold up the rest of its worker: its timers, sockets and woken
  tasks go to another worker meanwhile, except tasks pinned to that worker. When no other worker
  can take them (one worker, or every other one computing too), a spare worker starts and does,
  up to four in all; `workerCount()` counts each from then on. Past those, they wait for a worker
  to finish its computation. `VELT_RESCUE=0` turns this off (no spare starts, nothing moves away
  from a long computation), for deterministic measurement only: with it, a program's instruction
  count no longer depends on how its threads are scheduled (`bench/nightly.sh` sets it for the
  `async/` benchmarks), but one task that computes holds up everything on its worker. Tasks spawned together that compute (`spawn` in a loop, then `await Promise.all(...)`) start on idle workers well under a
  millisecond apart: once the first has computed for 50 µs, the others, not started yet, go to
  sleeping workers, one each (on Windows, whose timers are coarser, within about a millisecond).
  After many spawned tasks that each finished at once, the first such group may take up to a
  millisecond. `VELT_SCHEDULER=stealing` or
  `per-core` selects one of tokio's schedulers instead (for comparison;
  [`VELT_SCHEDULER`](../tooling/cli.md)); with `stealing`, `{ worker }` is checked but the task
  may run on any worker, and `currentWorker()` can change at any `await`.
- A pinned task can't borrow an idle core: CPU-bound code in it delays everything else on its
  worker. `scope.spawn` takes no placement yet.

```ts
import { channel, Channel } from "velt:channel";
import { workerCount, currentWorker } from "velt:task";

type Call = { key: string; reply: (n: i64) => void };

async function shard(inbox: Channel<Call>): Promise<void> {
  const counts = new Map<string, i64>();
  while (true) {
    const c = await inbox.receive();
    if (c == null) {
      return;
    }
    const n = (counts.get(c.key) ?? 0) + 1;
    counts.set(c.key, n);
    c.reply(n);
  }
}

async function caller(inbox: Channel<Call>): Promise<i64> {
  let n = 0;
  for (let i = 0; i < 1000; i++) {
    const { promise, resolve } = Promise.withResolvers<i64>();
    inbox.trySend({ key: "a", reply: resolve });
    n = await promise;
  }
  return n;
}

async function main() {
  const inboxes: Channel<Call>[] = [];
  for (let i = 0; i < workerCount(); i++) {
    inboxes.push(channel<Call>());
    spawn(shard(inboxes[i]), { worker: i });
  }
  // Next to shard 0: no call wakes another thread.
  const n = await spawn(caller(inboxes[0]), { worker: 0 });
  console.log(`${n} calls, main on worker ${currentWorker()}`);
  for (const c of inboxes) {
    c.close();
  }
}
```
