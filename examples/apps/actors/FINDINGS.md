# What Velt needs to host an actor runtime

Findings from porting the core of [@sigx/actors](https://github.com/signalxjs/actors) to Velt (this
directory). The port works: the demo, the in-process benchmarks and the HTTP endpoint pass
under the debug runtime's checking allocator and in `--release`. It needed a workaround for
every item in part 1.

The list is in three parts:

1. **Bugs.** Wrong code, crashes, compiler errors and internal compiler errors. Each has a
   minimal repro in [`findings/`](findings).
2. **Missing language and library features**, in order of impact on an actor runtime.
3. **Performance.** What the measurements say Velt's runtime should change.

## 1. Bugs (fix first)

Six of the eight bugs sit at the same crossroads: async code, calls through an interface or a
function value, and objects shared between two owners. That is the exact shape of an actor or
RPC runtime. Type-erased handlers are called through interfaces, and a dispatcher loop shares
objects with per-actor turn loops. The golden suite does not cover that combination.
`examples/apps/actors/demo.vlt` is a golden now, so it runs under the checking allocator.

| # | Bug | Effect | Repro | POC workaround |
|---|---|---|---|---|
| B1 | An async interface method with a `throws` clause, called through an interface value | SIGSEGV, in every build mode | [b1](findings/b1_async_interface_throws_crash.vlt) | No `throws` on async interface methods. Errors are returned as values (`Reply`). |
| B2 | An async method called through an interface value drops its receiver when the call ends | Use after free: the second call reads freed memory. Only async + interface receiver; concrete classes and sync methods are fine. | [b2](findings/b2_async_interface_drops_receiver.vlt) | Every dynamic seam (`Activation`, `Method`, `ActorStorage`) has sync methods that return the promise of a free async function. |
| B3 | A shared object passed to an async function as its last use in a loop body | The call moves the object without adding a reference, so each iteration gives one away. The object is freed while still in use: the shard's registry map, `Host`'s channel array, and a `JsonValue` that arrived as `[]`. | [b3](findings/b3_async_last_use_in_loop.vlt) | `touch(x)` after such calls (`runtime.vlt`), and arguments travel as JSON text. |
| B4 | An async closure forwards its object parameter to a captured async function value | It passes a copy, so the callee's changes are silently lost. | [b4](findings/b4_async_closure_forward_copies.vlt) | Typed methods are classes (`Method0/1/2`), not wrapping closures. |
| B5 | An async method called on a local copied out of an array element | Ownership inference never settles. It stops silently at its visit cap (`infer.rs`, debug_assert only) and leaves other functions with wrong modes: "cannot keep a copy of `f`, a borrowed function parameter" on legal code, including uncalled std (`setTimeout`, `Response.stream`). | [b5](findings/b5_inference_never_settles.vlt) | Turn queues are a `Deque` with `popFront`, and arrays are indexed in place. |
| B6 | `let x; try { x = … } catch { throw … }` read after an `await` | `--release`: "ICE: VIR verification failed after optimization: local may be used before it is assigned" | [b6](findings/b6_release_ice_try_assign.vlt) | Decoding moved into helper functions. |
| B7 | `JSON.parse<i64>` / `<f64>` / `<bool>` | The same release ICE, from a one-line program | [b7](findings/b7_release_ice_json_parse_number.vlt) | `parseOne<T>` parses `[json]` as `T[]`. |
| B8 | A field initializer that calls a throwing function | Compiler crash: "ICE: error thrown outside a throwing function" | [b8](findings/b8_field_initializer_throwing_call_ice.vlt) | Avoided. |

Also: a `Ticker` cannot be stopped before its pending tick. A shard that ticks once an hour
therefore takes an hour to shut down, so the POC ticks every second and checks the sweep
interval itself. `TickerStop` documents this, but it makes `Ticker` unfit for long periods.

Suggested guard rails, besides the fixes:

- **Make B5's cap fail loudly.** It should be an ICE in release too. A silent truncation turned
  one bad body into errors in unrelated std files.
- **Add a golden family for async × dynamic dispatch × sharing.** Each case would combine an
  interface or function-value receiver, an `async` method, `throws`, a receiver held in a field
  or map or array element, and a refcount above 1. Run them under `VELT_RT_DEBUG_ALLOC=1`.

## 2. Language and library features

Ordered by how much they shaped (or bent) the design.

1. **Non-escaping async closures that may modify their captures.** In sigx, an actor is
   `methods: (ctx) => ({ async increment(by) { ctx.state.count += by; … } })`: closures over a
   per-activation `ctx`. Velt rejects any async closure that modifies a captured object ("cannot
   mutate captured variable `ctx` in a spawned task") even when nothing spawns it. Velt needs a
   "local" (non-`Send`) async closure, inferred from never reaching `spawn`, a handler or a
   channel, that runs on its creator's task as started promises already do. Today every method
   takes `ctx` as a parameter.

2. **Generate an RPC surface from a class.** sigx infers everything from one `defineActor({...})`
   call: the dispatch table, argument decoding and a typed client proxy
   (`actor(Counter, key).increment(1)`). Velt has no reflection, decorators or macros, no rest
   parameters and no variadic generics. So the POC has:
   - hand-registered methods: `.method1<i64, i64>("increment", f)`;
   - one class per arity: `Method0`, `Method1` and `Method2`;
   - stringly-typed actor-to-actor calls:
     `decode<CounterState>(await ctx.call("Counter", key, "current", "[]"))`.

   Any one of these would close the gap:
   - compile-time reflection over a class's methods (`methodsOf<T>()` yielding name, parameter
     tuple type and result type);
   - a derive or macro facility;
   - variadic tuple generics (`<A extends unknown[]>(f: (ctx, ...a: A) => R)`) together with
     `JSON.parse<A>` on tuples.

3. **A cross-task one-shot reply.** Each call allocates a one-slot `Channel<Reply>`, sends, then
   receives and closes it. Velt needs a cheap one-shot: `Promise.withResolvers()` or
   `new Promise(resolve => …)` (both **Planned**) whose resolver can cross tasks.

4. **Channel `trySend` and `select` (or receive with a timeout).** An unbounded `send` never
   waits but is still async, so routing cannot be done atomically under a `Mutex`. That rules
   out a lock-based directory with one task per actor. Racing `receive()` against `sleep` loses
   messages: the losing async closure runs to completion and keeps the value. So a per-actor
   idle timeout needs a separate sweep.

5. **Task placement.** Velt needs `spawnOn(worker)` or a per-shard local executor (Tokio's
   `LocalSet`), and a way for `serve` to hand a connection to the shard that owns the key.
   - **Why:** on 4 cores, a call from a caller on another core runs 3.8× slower than on one
     pinned core (100k vs 380k calls/s at c=1). Every call wakes another worker twice; see
     part 3.
   - **Thread-per-core:** this is the Seastar/Orleans layout, and Velt's model (tasks own their
     data, values cross by move or copy) is already shaped for it.

6. **`JSON.decode<T>(value: JsonValue)`.** Arguments arrive inside a parsed envelope, and a typed
   argument costs `raw.stringify()` and then `JSON.parse<A>`. Related to B7.

7. **Shareable task handles.** A `Promise` has one owner, so a class holding the shards' join
   handles cannot be shared or passed to an async function. The POC's `Host` became a struct of
   channel handles with a `done` channel. Velt needs a cloneable `JoinHandle` (or
   `Promise.shared()`).

8. **Task-local context (AsyncLocalStorage).** sigx carries the call chain, deadline, principal
   and `traceparent` per call, and uses ALS for interleaved (`reentrant: 'always'`) turns. The
   POC threads `chain` and `deadline` through `ctx` by hand.

9. **Error mapping sugar.** Every `JSON.parse` inside a method is wrapped to turn `JsonError`
   into `ActorError`. This is because function types allow exactly one `throws` set. A
   `try`-expression or `catch`-and-map operator, or `cause`, would remove the `decode<T>`
   helpers.

10. **Smaller items:**
    - `s.length` is `usize`, while `slice` and `at` take `i64`. Casts are needed everywhere.
    - `x as T` binds looser than `>=`, so `a >= b as f64` parses as `(a >= b) as f64`. This is
      TS-compatible but surprising next to integer types.
    - No `process.memoryUsage()`; the POC reads `/proc/self/statm`.
    - No string hash in std; the POC writes its own FNV-1a for shard routing.
    - `Map` has no `new Map([[k, v]])` constructor.
    - Weak references (**Planned**, semantics stage 3) for caches that point back at their
      owners.

## 3. Performance

The measured numbers are in [README.md](README.md#results). What they say:

**The native HTTP endpoint is already where an actor host wants to be.** On the same 4-core VM
and the same wire protocol, Velt answered 17–23× the requests of @sigx/actors on Node (whose
serverFn pipeline costs ~4× over bare `node:http` here). It also beat bare `node:http` by 3.6–5.5×.

**In-process dispatch is where Velt loses, and the reasons are runtime costs, not compiled
code:**

| Cost per call | Where | Fix |
|---|---|---|
| 2 cross-thread wakeups (caller → shard → caller) | channels + Tokio scheduler | §2.5 task placement / local executors |
| 1 channel allocation + close for the reply | `Router.send` | §2.3 one-shot |
| JSON stringify + parse of the arguments | B3 workaround (args as text) | fix B3; then pass `JsonValue` |
| 1 promise allocation per turn loop start | `runTurns` per idle actor | fine once the above are gone |

On one pinned core the same code does 380k warm calls/s, against 794k for sigx's in-process
`host.dispatch`, whose arguments are JS values and are never serialized. So Velt's turn
machinery is within ~2× of a heavily tuned V8 path even with serialization on every call.
Cold activation is already 1.9× (all cores) to 6.9× (one core) faster than Node's, and
an idle actor costs ~2.6 KB of RSS against Node's ~5.5 KiB of V8 heap. Node's RSS figure is
higher; see the README.
