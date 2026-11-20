Follow-up to #3274 (part of #3126).

#3274 frees a spawned task that can never run again once every promise it drives is on its list of promises only their owner can run. One case it leaves, as its module docs say (std/lang/hooks/sched_task_lost.velt, "Not covered"): a promise handed to another task and dropped there, on another worker, while it is suspended. The drop cannot reach the task's list or give the promise up (both belong to the task's thread), and nothing polls the task afterwards, so the promise stays, and its task with it.

```ts
class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

// On worker 1: starts the promise (it runs and suspends under this task), then hands it on.
async function maker(): Promise<Holder> {
  const p = dead();
  await yieldNow();
  return new Holder(p);
}

// On worker 2: drops what it was given, unawaited.
async function dropper(h: Holder): Promise<number> {
  return 1;
}

async function main() {
  const h = await spawn(maker(), { worker: 1 });
  console.log(`dropped on worker 2: ${await spawn(dropper(h), { worker: 2 })}`);
}
```

`VELT_THREADS=4 VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1`: `blocks=25/16`, on main 8061c8631 and with #3274 alike (9 blocks: the promise and `maker`'s task).

A way: the cross-thread drop of such a promise wakes its task once (a wake needs no access to the list), and the poll that follows finds the promise unreachable and gives it up on the task's own thread; the task's end then applies as usual.
