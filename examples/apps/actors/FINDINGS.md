# What Velt needs to host an actor runtime

Findings from porting the core of [@sigx/actors](https://github.com/signalxjs/actors) to Velt
(this directory; tracked in [#182](https://github.com/velt-lang/velt/issues/182)). The first
round, in 2026-09, found eight bugs and ten gaps, and the port needed a workaround for each one.
They are fixed now, and the code no longer works around any of them. What remains are features
Velt does not have yet and the cost of in-process dispatch.

## 1. Bugs: all fixed

Six of the eight bugs sat at the same crossroads: async code, calls through an interface or a
function value, and objects with more than one owner. That is the shape of an actor or RPC
runtime. The fixes came with goldens for that combination (#202), and `demo.vlt` is a golden
too.

| # | Bug | Issue | What the port does now |
|---|---|---|---|
| B1 | An async interface method with `throws`, called through an interface value, crashed | #193 | `Method<S>` and `Activation` are async and throw `ActorError` |
| B2 | An async method called through an interface value dropped its receiver | #194 | `Live.invoke` and `MemoryStorage.load/save` are async methods |
| B3 | A shared object passed to an async call as its last use in a loop body lost a reference | #195 | No `touch()` calls. The arguments cross to the shard as a `Value`, not as JSON text |
| B4 | An async closure passed a copy when it forwarded its object parameter to an async function value | #196 | Methods are wrapping closures, not `Method0/1/2` classes |
| B5 | Ownership inference never settled for an async method called on a local copied out of an array | #197 | Turn queues are arrays with `shift()` |
| B6 | Release ICE: a variable assigned in `try` and read after an `await` | #198 | Decoding is inline `let x; try { x = … } catch { throw … }` |
| B7 | Release ICE: `JSON.parse<i64>` | #199 | `JSON.parse<T>` and `Value.as<T>()` directly |
| B8 | ICE: a field initializer that calls a throwing function | #200 | — |

The small library gaps of the first round are fixed and used too: `Ticker.stop()` takes effect
at once (#201), `Promise.withResolvers` replaces a one-slot reply channel per call (#203),
`Channel.trySend` (#204), `new Map(entries)` (#205), `process.memoryUsage()` (#206) and
`fnv1a64` from `velt:hash` (#207).

This round found no new compiler bugs.

## 2. Open: language and library features

Ordered by how much they shape the design.

1. **Local async closures that may change their captures** (#208). In sigx an actor is
   `methods: (ctx) => ({ async increment(by) { ctx.state.count += by; … } })`: closures over a
   per-activation `ctx`. Velt rejects an async closure that changes a captured object, even when
   nothing spawns it, so every method takes `ctx` as a parameter.
2. **A typed RPC surface** (#209). sigx infers the dispatch table, argument decoding and a typed
   client proxy (`actor(Counter, key).increment(1)`) from one `defineActor` call. Without
   variadic tuple generics or `Parameters<F>` / `ReturnType<F>`, methods are registered per
   arity (`method0/1/2`), and actor-to-actor calls are untyped:
   `ctx.call("Counter", key, "current", JsonValue.array())`.
3. **Task placement** (#211). There is no way to pin a task to a worker, or to have `serve` hand
   a request to the worker that owns the key. This is the main in-process cost (section 3).
4. **Receive with an `AbortSignal`** (#210). A `receive()` that loses a race keeps its message,
   so a per-actor idle timeout needs a separate sweep.
5. **Shareable promises** (#212). A promise has one owner, so `Host` is a struct of channel
   handles with a `done` channel rather than a class holding the shards' promises. The bench
   starts its callers with a channel of deadlines where one shared "go" promise would do.
6. **`AsyncContext.Variable`** (#213). The call chain and deadline are threaded through `ctx`
   by hand. sigx carries them, and the principal and trace context, in AsyncLocalStorage.
7. **`process.cpuUsage()`** (#636, new). The comparison measures CPU time per call. Node reads
   it in process; the Velt side has to time the whole process from outside.
8. **Lengths as `usize`** (#214). `s.length` is `usize` while `slice` and `at` take `i64`, so
   casts are everywhere.

## 3. Performance

The numbers are in [README.md](README.md#results). In short:

- **HTTP:** Velt spends 4–5× less CPU per request than @sigx/actors on Node (12× less in user
  mode), and less than a bare `node:http` handler. It needs a tenth of sigx's memory.
- **In process:** Velt spends 2.2–6× the CPU per call of sigx's `host.dispatch` on one thread,
  and 4–7× with 4 shards. The instruction count is the same with 1 shard or 4 (11–13k per call),
  so the gap between them is thread wakeups.

Where an in-process call's instructions go (callgrind, one thread, warm actor), and what would
remove them:

| Cost per call | Share | Fix |
|---|---:|---|
| Envelope through the shard's channel, one byte at a time | ~20% | #635 (new): copy items whole. Registry lock and `Arc` clone per operation: #146 |
| Allocation: the turn loop's promise for an idle actor, the `withResolvers` slot and settlers, the envelope, the chain array | ~13% | Fewer allocations per call in the runtime; a cheaper one-shot reply |
| Strings: the `type/key` id and the `{"data":…}` reply body, formatted per call | ~8% | In the port: intern ids, and pass replies as values instead of wire text |
| Two cross-thread wakeups per call with several shards | 2–3× CPU, not instructions | #211 task placement |

A finding about measuring, not about Velt: on a machine at 100% load, the OS deschedules
threads for 50–180 ms at a time, Node's as much as Velt's. The bench used to compute its
deadline before spawning its callers, so one such stall during the spawn loop could swallow a
whole 200 ms pass and report 0 calls/s. That is the `warm c=64 = 0 ops/s` of an earlier smoke
run. The callers now start on a channel once all of them exist.
