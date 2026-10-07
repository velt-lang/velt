# actors

A proof of concept: the core of [@sigx/actors](https://github.com/signalxjs/actors) written in
Velt. It has addressable `(type, key)` actors that activate on their first call and run one turn
at a time. They persist through an etag compare-and-swap, call each other with deadlock
detection, and deactivate when idle. The wire endpoint is sigx's, so one client and one load
generator drive both runtimes. The `node/` directory holds the same actors on @sigx/actors for
the comparison.

**What Velt needs to make this real is in [FINDINGS.md](FINDINGS.md)**: eight bugs with minimal
repros in [`findings/`](findings), ten language and library gaps, and what the measurements say
about the runtime.

```sh
velt run demo.vlt                       # the scripted walk below (golden: demo.out)
velt build --release
./target/velt/actors serve --port 5199  # POST /_sigx/actor/{Type}/{method} {"args":[key, ...]}
./target/velt/actors bench              # in-process benchmarks (src/bench.vlt)
bench/inproc.sh && bench/http.sh        # the comparison with @sigx/actors (see Results)
```

```
$ velt run demo.vlt
-- calls and persistence
POST Counter/increment {"args":["a",1]} -> 200 {"data":1}
POST Counter/increment {"args":["a",2]} -> 200 {"data":3}
POST Counter/sleep {"args":["a"]} -> 200 {"data":true}
POST Counter/increment {"args":["a",3]} -> 200 {"data":6}
-- actor to actor
POST Counter/increment {"args":["b",10]} -> 200 {"data":10}
POST Counter/pull {"args":["a","b"]} -> 200 {"data":16}
POST Counter/loop {"args":["a","b"]} -> 500 {"error":{"status":500,"message":"call cycle: Counter/a -> Counter/b -> Counter/a","data":{"kind":"deadlock"}}}
-- errors
POST Counter/increment {"args":["a",-1]} -> 400 {"error":{"status":400,"message":"increment: by must not be negative","data":{"kind":"bad-request"}}}
…
```

## Design

The host is **sharded, one shard per core**:

- A shard is one task that owns a slice of the actor directory (`hash(type/key) % shards`).
- Inside it, turns of different actors interleave at their `await`s, like Node's single thread.
  No actor state is touched by two threads, and nothing takes a lock.
- A caller on any thread, such as an HTTP handler, a benchmark task or another actor's turn,
  reaches the owning shard through its channel. It gets the answer on a one-slot reply
  channel.

| @sigx/actors | here |
|---|---|
| `defineActor({ type, state, methods: (ctx) => ({…}) })` | `new ActorDef<S>(type, init, methodsFactory)`. The factory returns `new Methods<S>().method1<A, R>("name", async (ctx, a) => …)`, built once per activation like sigx's. |
| `ctx.state`, `await ctx.save()`, `ctx.deactivate()` | Same names. `save()` persists when the turn ends, before the caller is answered. |
| `ctx.actor(Def, key).method(…)` | `ctx.call(type, key, method, argsJson)`, which returns JSON (untyped: FINDINGS §2.2). |
| Turn queue, single activation per id | A `Deque` of envelopes per `Slot` and a turn loop per busy actor, in the shard's directory. |
| `ActorStorage` with etag CAS, `memoryStorage` | `ActorStorage` and `MemoryStorage`. A conflict answers 409 and drops the activation. |
| Deadlines, deadlock detection, 404/400/409/429/504 | `x-sigx-deadline-ms` (30 s default), with expired turns skipped at dequeue. Call chains detect deadlocks. Errors use the same `{"error":{status,message,data:{kind}}}` bodies. |
| Idle sweep (`idleAfterMs`, `sweepIntervalMs`) | Per shard, on a `Ticker`. |

Not ported: streams and live reads, reminders, timers, topics, workers, jobs, write-behind,
migrations, auth and principals, clustering and placement, and file storage. None of them is
blocked by something new: they need the same primitives the list in FINDINGS.md asks for.

Every workaround in the source is marked with the FINDINGS.md item it works around: `B1`–`B8`
for the bugs, `§2.n` for the gaps.

## Results

All runs are on one 4-vCPU Linux VM (Intel Xeon 2.8 GHz, 15 GB), with Node 22.22 and
@sigx/actors at `6a94345` built for production. Velt is at `5320d29` with `velt build --release`
(LLVM). The VM is noisy: sigx's harness flagged it as busy even when nothing else ran. Read
differences under ~20% as noise. The raw outputs are in [`bench/results/`](bench/results).

### HTTP, the same wire requests (`bench/http.sh`, wrk, 1000 keys, 10 s per level)

| | conns | Velt req/s | sigx on Node req/s | bare `node:http` req/s | Velt / sigx |
|---|---:|---:|---:|---:|---:|
| `Tiny.noop`, whole machine | 16 | 45.4k | 3.04k | 12.9k | 15× |
| | 64 | 62.0k | 2.96k | 12.7k | 21× |
| | 256 | 65.8k | 2.88k | 12.5k | 23× |
| `Counter.increment` (saves), whole machine | 64 | 57.3k | 2.57k | — | 22× |
| `Tiny.noop`, server pinned to 1 core | 16 | 38.0k | 2.26k | 11.7k | 17× |
| | 64 | 40.1k | 2.56k | 12.4k | 16× |
| `Counter.increment`, 1 core | 64 | 35.6k | 2.38k | — | 15× |

Latency and memory:

- At 64 connections on the whole machine, p50/p99 are 0.80/3.3 ms for Velt and 20/262 ms for
  sigx.
- Peak RSS is 21–34 MB for Velt, 218–305 MB for sigx and 69–96 MB for bare node.

**Bare `node:http` is the calibration.** sigx's serverFn pipeline costs ~4× on this machine,
and Velt's whole actor path still runs 3–5× faster than Node's empty handler. "Whole machine"
means the server may use every core while wrk (2 threads) competes for them. Node uses one.
"1 core" pins the server to CPU 0 (`--shards 1`) and wrk to CPUs 2–3.

### In process, no HTTP (`bench/inproc.sh`)

sigx runs its own benchmark scenarios (`pnpm bench:run`, `host.dispatch`, 3 interleaved rounds,
medians). Velt runs their equivalents in `src/bench.vlt`.

| Scenario | sigx (1 thread) | Velt, 4 shards | Velt, 1 core |
|---|---:|---:|---:|
| `dispatch/warm-actor` c=1 / 64 / 512, calls/s | 794k / 766k / 709k | 101k / 244k / 168k | 380k / 399k / 361k |
| `dispatch/fan-out-actors` (1000 keys) c=1 / 64 / 512 | 672k / 663k / 439k | 67k / 281k / 319k | 230k / 278k / 187k |
| `activation/cold-cycle` (activate and deactivate a new key), cycles/s | 19.7k | 36.8k | 135.9k |
| Activations/s at c=64, 100k actors | 39.7k | 43.5k | 34.2k |
| Memory per idle `Tiny` actor | 4.4–7.2 KB RSS, 5.6 KB V8 heap | 2.6 KB RSS | 2.6 KB RSS |

**Why in-process dispatch loses.** sigx's in-process path passes JS values on one thread and
never serializes. Every Velt call pays for:

- a channel hop to the owning shard and back: two cross-thread wakeups with 4 shards;
- a reply-channel allocation;
- a JSON round trip of the arguments, a workaround for B3.

That is why one pinned core beats four cores at c=1. FINDINGS.md §3 lists what would remove each
cost.

## Files

| File | What |
|---|---|
| `src/runtime.vlt` | The runtime: definitions, activations, storage, shards, router, host |
| `src/app.vlt` | The actors: `Counter` (examples/counter's) and `Tiny` (the sigx benchmark actor) |
| `src/wire.vlt` | The sigx wire endpoint on `velt:http` |
| `src/bench.vlt` | In-process benchmarks, mirroring sigx's scenarios |
| `src/main.vlt` | `actors serve` / `actors bench` |
| `demo.vlt` / `demo.out` | The scripted walk above (golden: `cargo test -p veltc --test golden`) |
| `node/server.mjs` | The same actors and endpoint on @sigx/actors (`SIGX_ACTORS=…/packages/actors`) |
| `node/mem.mjs` | RSS per activation on @sigx/actors, the metric Velt's bench reports |
| `node/bare.mjs` | Plain `node:http`, for calibration |
| `bench/http.sh`, `bench/post.lua` | HTTP comparison with wrk |
| `bench/inproc.sh` | In-process comparison (runs sigx's own scenarios) |
| `bench/results/` | The outputs quoted above |
| `FINDINGS.md`, `findings/*.vlt` | What Velt needs, with minimal repros |
