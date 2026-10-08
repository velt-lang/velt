# velt:channel

`import { channel, Channel, ChannelClosed } from "velt:channel"`. Typed queues between tasks,
for producer/consumer pipelines and worker pools. Any number of tasks may send and receive
through one channel.

- `channel<T>(capacity = 0): Channel<T>`: unbounded with `0`, else holding at most `capacity`
  values. On a full channel, `send` waits until a receiver makes room (backpressure). A
  negative `capacity` panics.
- `Channel<T>`:
  - `send(value: T): Promise<void>` queues the value. It throws `ChannelClosed` if the channel
    is closed, also while waiting; the value is dropped then.
  - `trySend(value: T): bool` queues the value if the channel has room now, without waiting.
    It returns false if a bounded channel is full or the channel is closed; the value is
    dropped then. Being synchronous, it can be called where `await` can't, such as inside a
    `Mutex`'s `with` callback (an unbounded channel is never full).
  - `receive(): Promise<T | null>` returns the oldest value, waiting for one. It returns `null`
    once the channel is closed and empty.
  - `tryReceive(): T | null` returns the oldest value if one is queued, without waiting.
  - A channel is an `AsyncIterable<T>`: `for await (const v of ch)` receives values until the
    channel is closed and drained, then ends. Leaving the loop early (`break`, `return`, an
    error) does **not** close the channel: other tasks may still be receiving from it, and the
    values still queued stay receivable. Call `close()` when the stream is over. The loop
    costs what a `receive()` loop costs (no allocation per value).
  - `close()` ends the stream. Closing twice does nothing.
  - `closed` and `length` (the number of queued values).
- A sent value crosses to the receiver like a `spawn` argument: it moves when the sender no
  longer uses it, and is deep-copied otherwise, so tasks never share an object. Changing an
  object after sending it doesn't affect the receiver's copy.
- `Channel<T>` is a handle struct, like `TcpStream` ([Handles](README.md#conventions)). Copies
  share one channel, so pass it to producers and consumers, `spawn`ed tasks included, without
  `shared(...)`.
- Close a channel when you're done with it. A closed channel is freed once it is drained. A
  channel abandoned with values still queued (one per connection, say) keeps them until the
  program ends, since any copy of the channel may still receive them; they are dropped only
  then (when `main` succeeds; on WebAssembly they are not dropped, the instance's memory goes
  with it). To free them sooner, drain it: close it and receive until you get `null`. A
  channel that is never closed lives until the program ends.

```ts
import { channel, Channel, ChannelClosed } from "velt:channel";

async function worker(jobs: Channel<string>, results: Channel<i64>): Promise<void> throws ChannelClosed {
  for await (const job of jobs) {     // ends once `jobs` is closed and drained
    await results.send(job.length as i64);
  }
}

async function main() {
  const jobs = channel<string>(10);   // bounded: senders wait when 10 jobs are queued
  const results = channel<i64>();
  const workers: Promise<void, ChannelClosed>[] = [];
  for (let i = 0; i < 4; i++) {
    workers.push(spawn(worker(jobs, results)));
  }
  for (const name of ["a.txt", "notes.md", "report.pdf"]) {
    await jobs.send(name);
  }
  jobs.close();                       // workers finish once the queue is drained
  await Promise.all(workers);
  results.close();
  let total = 0;
  for await (const n of results) {
    total += n;
  }
  console.log(total); // 23
}
```

Notes: when several tasks wait to receive, each value goes to one of them. A pending `send` or
`receive` that is dropped gives up its place (a dropped `send` drops its value).
