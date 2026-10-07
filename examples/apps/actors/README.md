# actors

The core of [@sigx/actors](https://github.com/signalxjs/actors) (virtual actors) written in
Velt, to find out what Velt needs to host an actor runtime and how it compares with Node
([#182](https://github.com/velt-lang/velt/issues/182)). It has addressable `(type, key)` actors
that activate on their first call and run one turn at a time. They persist through an etag
compare-and-swap, call each other with deadlock detection, and deactivate when idle. The wire
endpoint is sigx's, so one client and one load generator drive both runtimes. The `node/`
directory holds the same actors on @sigx/actors for the comparison.

`demo.vlt` is a golden (`demo.out`), so the end-to-end tests run it in debug and release mode.
[FINDINGS.md](FINDINGS.md) lists what the port found in Velt: what is fixed and what is still
open.

## Running it

```sh
velt run demo.vlt                       # the scripted walk below (golden: demo.out)
velt build --release
./target/velt/actors serve --port 5199  # POST /_sigx/actor/{Type}/{method} {"args":[key, ...]}
./target/velt/actors bench              # in-process throughput and latency (src/bench.vlt)
./target/velt/actors calls --scenario warm --c 64 --n 100000   # a fixed number of calls
bench/cpu.sh inproc && bench/cpu.sh http   # CPU time per call against @sigx/actors (Results)
```

`serve`, `bench` and `calls` take `--shards N` (default: one per core). The comparison scripts
need Node 22.18 or newer and a built checkout of signalxjs/actors in `SIGX_ACTORS_REPO`
(`pnpm install && pnpm build`). `bench/cpu.sh http` also needs [oha](https://github.com/hatoo/oha).

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
  reaches the owning shard through its channel. The shard answers by resolving the caller's
  `Promise.withResolvers` promise.

| @sigx/actors | here |
|---|---|
| `defineActor({ type, state, methods: (ctx) => ({…}) })` | `new ActorDef<S>(type, init, methodsFactory)`. The factory returns `new Methods<S>().method1<A, R>("name", async (ctx, a) => …)`, built once per activation like sigx's. Methods take `ctx` as a parameter because an async closure cannot change what it captures yet ([#208](https://github.com/velt-lang/velt/issues/208)). |
| `ctx.state`, `await ctx.save()`, `ctx.deactivate()` | Same names. `save()` persists when the turn ends, before the caller is answered. |
| `ctx.actor(Def, key).method(…)` | `ctx.call(type, key, method, args)`, which returns JSON. It is untyped until Velt has variadic tuple generics or `Parameters<F>` ([#209](https://github.com/velt-lang/velt/issues/209)). |
| Turn queue, single activation per id | An array of envelopes per `Slot` and a turn loop per busy actor, in the shard's directory. |
| `ActorStorage` with etag CAS, `memoryStorage` | `ActorStorage` and `MemoryStorage`. A conflict answers 409 and drops the activation. |
| Deadlines, deadlock detection, 404/400/409/429/504 | `x-sigx-deadline-ms` (30 s default), with expired turns skipped at dequeue. Call chains detect deadlocks. Errors use the same `{"error":{status,message,data:{kind}}}` bodies. |
| Idle sweep (`idleAfterMs`, `sweepIntervalMs`) | Per shard, on a `Ticker`. |

Not ported: streams and live reads, reminders, timers, topics, workers, jobs, write-behind,
migrations, auth and principals, clustering and placement, and file storage.

## Results

**Read these as relative, not absolute.** They were measured on 2026-10-07 at `2cce7b99`, on a
Windows i9-12900HK (20 threads) that sat at 100% load from other builds the whole time. The OS
descheduled even a busy loop for 50–180 ms at a time. So the tables report **CPU time per call**
(user + system, all threads): it is distorted by load much less than throughput. Velt is built
with `velt build --release` (LLVM), Node is 22.22, and @sigx/actors is `6a94345` built for
production. Each figure is the lower of two runs. Throughput and latency on this machine are
noise, and need re-measuring on a quiet one. The raw outputs are in
[`bench/results/2026-10-windows`](bench/results/2026-10-windows).

### HTTP, the same wire requests (`bench/cpu.sh http`)

oha sends 40,000 requests round robin over 1000 keys after a warmup of 8,000. The table shows
the server's CPU µs per request, so lower is better.

| | conns | Velt, all cores | Velt, 1 shard | sigx on Node | bare `node:http` |
|---|---:|---:|---:|---:|---:|
| `Tiny.noop` | 16 | 123 | 126 | 480 | 139 |
| | 64 | 99 | 106 | 515 | 140 |
| | 256 | 104 | 113 | 489 | 153 |
| `Counter.increment` (saves) | 64 | 125 | 120 | 534 | — |
| `Tiny.noop`, user-mode CPU only | 64 | 36 | | 453 | 58 |

- **Velt spends 4–5× less CPU per request than @sigx/actors, and 12× less in user mode.** It
  also beats a bare `node:http` handler that does no actor work, by 1.4× in all and 1.6× in user
  mode.
- Most of Velt's CPU per request on Windows goes to the kernel: about 65 of the 100 µs are
  loopback TCP.
- Peak working set is 13–28 MB for Velt, 211–227 MB for sigx and 61–76 MB for bare Node.
- Loaded-machine throughput (for orientation only): Velt 17–53k req/s, sigx 0.5–1.9k, bare
  Node 3.5–7.8k.

### In process, no HTTP (`bench/cpu.sh inproc`)

`actors calls` and `node/calls.mjs` make the same calls: `c` callers call `Tiny.noop`, on one
warm key or round robin over 1000 warm keys. sigx goes through `host.dispatch`, with the fixture
of its own benchmarks. Each configuration runs with 20,000 calls and with 220,000. The table
shows Δ CPU / Δ calls in µs per call, which cancels startup, so lower is better.

| Scenario | c | Velt, 4 shards | Velt, 1 thread | sigx (1 thread) |
|---|---:|---:|---:|---:|
| warm actor (one key) | 1 | 9.1 | 3.8 | 1.5 |
| | 64 | 7.7 | 3.2 | 1.4 |
| | 512 | 8.3 | 3.3 | 0.9 |
| 1000 keys | 1 | 10.6 | 4.1 | 1.4 |
| | 64 | 5.8 | 4.1 | 0.9 |
| | 512 | 5.9 | 5.0 | 0.9 |

Instruction counts (cachegrind, Linux in WSL, same calls) give **11–13k instructions per call
for Velt, with 1 shard or 4**. So the extra CPU with 4 shards is the cost of waking and parking
threads (two cross-thread wakeups per call), not more work. Node could not be counted: V8 dies
under valgrind.

**In-process dispatch is where Velt still loses to Node:** 2.2–6× the CPU per call on one
thread, and 4–7× with 4 shards. A callgrind profile of one warm call on one thread puts the cost
in the runtime, not in compiled code:

- **The channel to the shard: about 20%.** Moving the ~100-byte envelope through the channel copies
  it one byte at a time ([#635](https://github.com/velt-lang/velt/issues/635)), 1,400
  instructions in `Chan::try_pop` alone. The registry lock comes on top
  ([#146](https://github.com/velt-lang/velt/issues/146)).
- **Allocation: about 13%.** This covers the promise of the turn loop that starts for an idle
  actor, the `withResolvers` slot and its two settler closures, the `Envelope`, and the chain
  array.
- **Strings: about 8%.** The `type/key` id is formatted on every call, and so is the `{"data":…}`
  reply body.
- **Promise machinery** (start, poll, settle): the rest of the runtime share.

With 4 shards, the two wakeups per call dominate: the shard and the caller sit on different
workers. Fixing that needs task placement ([#211](https://github.com/velt-lang/velt/issues/211)).

### Earlier results (2026-09, Linux VM)

[`bench/results/2026-09-linux-vm`](bench/results/2026-09-linux-vm) holds the first round. It ran
at `5320d29` on a 4-vCPU Linux VM, measured throughput with wrk and sigx's own benchmark runner,
and is quoted in #182. Over HTTP, Velt answered 15–23× the requests per second of
@sigx/actors. In process, one pinned Velt core ran 380k warm calls/s against sigx's 794k. That
round still carried the bug workarounds, among them a JSON round trip of every call's arguments.

## Files

| File | What |
|---|---|
| `src/runtime.vlt` | The runtime: definitions, activations, storage, shards, router, host |
| `src/app.vlt` | The actors: `Counter` (examples/counter's) and `Tiny` (the sigx benchmark actor) |
| `src/wire.vlt` | The sigx wire endpoint on `velt:http` |
| `src/bench.vlt` | `actors bench` (throughput and latency) and `actors calls` (fixed work) |
| `src/main.vlt` | `actors serve` / `bench` / `calls` |
| `demo.vlt` / `demo.out` | The scripted walk above (golden: `cargo test -p veltc --test golden`) |
| `node/server.mjs` | The same actors and endpoint on @sigx/actors (`SIGX_ACTORS=…/packages/actors`) |
| `node/calls.mjs` | `actors calls` on @sigx/actors' `host.dispatch` |
| `node/mem.mjs` | RSS per activation on @sigx/actors |
| `node/bare.mjs` | Plain `node:http`, for calibration |
| `bench/cpu.sh`, `bench/cpu.ps1` | CPU time per call, in process and over HTTP (Linux, and Windows with Git Bash) |
| `bench/http.sh`, `bench/post.lua`, `bench/inproc.sh` | The throughput comparison of the first round (Linux, wrk, sigx's runner) |
| `bench/results/` | Raw outputs of both rounds |
| `FINDINGS.md` | What the port found: fixed bugs, open gaps, performance |
