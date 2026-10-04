# velt:task

`import { AbortController, AbortSignal, timeout, taskScope } from "velt:task"`. Cancellation
with TypeScript's `AbortController` / `AbortSignal`, timeouts, and task scopes (structured
concurrency).

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
