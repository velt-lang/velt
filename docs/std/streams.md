# Streams

`ReadableStream`, `ReadableStreamDefaultReader` and `ReadableStreamDefaultController` are
globals, as in Node (no import; the module is `velt:streams`). A stream is made from a source
whose `start`, `pull` and `cancel` callbacks the stream calls in the order the WHATWG Streams
standard gives, and is read with a reader, with `for await`, or as a `Response` body.

```ts
function numbers(): ReadableStream<number> {
  let n = 0;
  return new ReadableStream<number>({
    pull(controller) {
      n++;
      if (n > 3) {
        controller.close();
      } else {
        controller.enqueue(n);
      }
    },
  });
}

async function main() {
  const reader = numbers().getReader();
  for (;;) {
    const r = await reader.read();
    if (r.done) {
      break;
    }
    console.log(r.value); // 1, 2, 3
  }
  for await (const x of numbers()) {
    console.log(x); // 1, 2, 3
  }
  const page = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode("<p>hi</p>"));
      controller.close();
    },
  });
  console.log(await new Response(page).text()); // <p>hi</p>
}
```

- **`new ReadableStream<R>(source?, strategy?)`**. The source's `start(controller)` runs in the
  constructor; `pull(controller)` runs once `start` has settled (a microtask later) and again
  whenever the queue is below `strategy.highWaterMark` (default 1; 0 pulls only for a waiting
  read), never two at once; `cancel(reason)` runs when a consumer cancels. Each may be `async`
  (the stream waits for its promise), and an error one throws errors the stream.
- **The controller**: `enqueue(chunk)` (a waiting read gets it, or it is queued), `close()`
  (reads get the queued chunks, then `done`), `error(e)` (queued chunks are dropped and reads
  reject with `e`), and `desiredSize`. `enqueue` or `close` after `close` throws `TypeError:
  Invalid state: Controller is already closed`.
- **Reading**: `stream.getReader()` locks the stream (`locked`; a second `getReader()` throws
  `TypeError: Invalid state: ReadableStream is locked`). `reader.read()` gives `{ value, done:
  false }`, or `{ done: true }` once the stream is closed, and rejects once it errored;
  `releaseLock()` lets go (a waiting read rejects with `Invalid state: Releasing reader`);
  `reader.cancel(reason)` and `reader.closed` are as in JS. `read()` and `cancel()` do their
  work when they are called, as in JS, not when they are awaited.
- **`for await (const chunk of stream)`** reads to the end; leaving the loop early (`break`,
  `return`, a throw) cancels the stream and releases it.
- **`ReadableStream.from(iterable)`** reads an async iterable (an async generator); an error
  it throws reaches the reader as an `Error` with its text.
- **Bodies**: `new Response(stream)` and `new Request(url, { body: stream })` take a
  `ReadableStream<Uint8Array>`, and `res.body` / `req.body` are one ([fetch](fetch.md)), pulled
  only as they are read.
- `console.log(stream)` prints what Node prints: `ReadableStream { locked: false, state:
  'readable', supportsBYOB: false }`.

Differences from Node:

- Velt has no `undefined`: a finished read is `{ done: true }`, which prints without `value:
  undefined`, and a `cancel` reason is an `Error`, a `string` or `null`.
- An `await` resumes as soon as its promise settles, before callbacks queued earlier
  ([Async](../reference/async.md)), so code after `await reader.read()` can run before a `pull`
  that Node would have called first (the order of the `pull` calls themselves is Node's).
- Not supported yet: `WritableStream`, `TransformStream`, `pipeTo`, `pipeThrough`, `tee`,
  byte streams (`type: "bytes"`, BYOB readers), a strategy's `size` function, `values({
  preventCancel })`, and an `async` source callback's synchronous part running at once (it runs
  when the stream waits for it, a step later than in Node).
