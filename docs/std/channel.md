# velt:channel

`import { channel, Channel, ChannelClosed } from "velt:channel"`. Typed queues between tasks,
for producer/consumer pipelines and worker pools. Any number of tasks may send and receive
through one channel.

- `channel<T>(capacity = 0): Channel<T>`: unbounded with `0`, else holding at most `capacity`
  values. On a full channel, `send` waits until a receiver makes room (backpressure).
- `Channel<T>`:
  - `send(value: T): Promise<void>` queues the value. It throws `ChannelClosed` if the channel
    is closed, also while waiting; the value is dropped then.
  - `receive(): Promise<T | null>` returns the oldest value, waiting for one. It returns `null`
    once the channel is closed and empty.
  - `tryReceive(): T | null` returns the oldest value if one is queued, without waiting.
  - `close()` ends the stream. Closing twice does nothing.
  - `closed` and `length` (the number of queued values).
- A sent value crosses to the receiver like a `spawn` argument: it moves when the sender no
  longer uses it, and is deep-copied otherwise, so tasks never share an object. Changing an
  object after sending it doesn't affect the receiver's copy.
- `Channel<T>` is a handle struct, like `TcpStream` ([Handles](README.md#conventions)). Copies
  share one channel, so pass it to producers and consumers, `spawn`ed tasks included, without
  `shared(...)`.
- Close a channel when you're done with it. A closed channel is freed once it is drained;
  values left in a closed channel that nobody drains are not dropped. A channel that is never
  closed lives until the process exits.

```ts
import { channel, Channel, ChannelClosed } from "velt:channel";

async function worker(jobs: Channel<string>, results: Channel<i64>): Promise<void> throws ChannelClosed {
  while (true) {
    const job = await jobs.receive();
    if (job == null) {
      return;
    }
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
  while (true) {
    const n = results.tryReceive();
    if (n == null) {
      break;
    }
    total += n;
  }
  console.log(total); // 23
}
```

Notes: when several tasks wait to receive, each value goes to one of them. A pending `send` or
`receive` that is dropped gives up its place (a dropped `send` drops its value).
