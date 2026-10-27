# Design: Velt on Cloudflare Workers

Status: proposed ([#1734](https://github.com/velt-lang/velt/issues/1734)). Sub-issues: host
imports [#1735](https://github.com/velt-lang/velt/issues/1735), glue and target
[#1736](https://github.com/velt-lang/velt/issues/1736), async over host promises
[#1737](https://github.com/velt-lang/velt/issues/1737), `velt:cloudflare`
[#1738](https://github.com/velt-lang/velt/issues/1738), tests and budgets
[#1739](https://github.com/velt-lang/velt/issues/1739), the registry
[#1740](https://github.com/velt-lang/velt/issues/1740).

A Velt program that serves HTTP with `velt:http` builds to a Cloudflare Worker and runs there
unchanged:

```ts planned
import { serve } from "velt:http";

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    return Response.json({ path: url.pathname });
  });
}
```

```sh
velt build --release --target cloudflare app.vlt   # target/velt/app-worker/: app.wasm, worker.mjs, wrangler.toml
npx wrangler deploy --config target/velt/app-worker/wrangler.toml
```

The first large user is the public package registry
([#1325](https://github.com/velt-lang/velt/issues/1325)). Its Cloudflare host runs on Workers,
R2, D1 and Durable Objects, which are free to start and then $5/month.

## Velt, not Rust

Velt is moving off Rust onto Velt (the SQLite engine
[#1726](https://github.com/velt-lang/velt/issues/1726), OS calls from Velt
[#1716](https://github.com/velt-lang/velt/issues/1716), the JSON engine
[#1655](https://github.com/velt-lang/velt/issues/1655)). This work follows the same rule:

- The request/response bridge, timers, outbound `fetch` and `velt:cloudflare` are Velt code that
  calls host functions declared in Velt. Packages can do anything std does here.
- Where Velt can't express something, we add the language feature (section 1) and do not write
  the code in Rust.
- New Rust is limited to the executor's turn and wake entry (section 5), because the wasm
  executor itself is Rust today. The [table at the end](#what-is-rust-and-what-is-velt) lists
  each Rust piece and how it moves to Velt.

## Starting point

- `--target wasm32-unknown-unknown` builds a module plus JS glue (`crates/velt_rt_wasm/js/velt_web.mjs`).
  The runtime `velt_rt_wasm` is single-threaded, with a current-thread executor
  (`task/executor.rs`). Host services come from the `velt` wasm import module
  (`platform.rs`: `write`, `exit`, `now_ms`, `date_ms`), and only Rust can declare them.
- The glue runs `main` to completion and returns an exit code. `velt_rt_block_on` sleeps until
  the next timer, which busy-waits in the browser (`platform::sleep_ms`), and stops when nothing
  can wake a task.
- `velt:http`'s `serve(opts, handler)` hands the runtime a `VeltHandler` (init, poll, drop,
  state size and alignment, environment), built by `__intrinsic_http_handler` from a closure
  `(raw: u64) => Promise<u64>` (std/http.vlt). On wasm `velt_rt_http_serve` doesn't link, and
  every `velt_rt_http_req_*` accessor is a stub that stops the program (`server_request.rs`).
  `fetch` fails with `ENOTSUP` (`fetch.rs`).
- Handlers take web-standard `Request` and return `Response` (std/fetch), the same shape as a
  Worker's `fetch(request, env, ctx)`.
- `declare function` binds a C-ABI symbol. Only std may declare `velt_rt_*`, and a package may
  declare only exports of its own native library (reference/modules.md "Runtime
  declarations").
- Velt's `async` functions compile to state machines that the executor polls. Waiting on the
  host therefore needs no stack switching (no Asyncify, no JSPI): a task that waits returns
  `Pending`, the executor returns to JS, and JS calls back in later. This is what makes the
  design cheap.

## Cloudflare's limits

From Cloudflare's documentation, October 2026; re-check them before setting budgets.

| Limit | Free | Paid |
|---|---|---|
| Worker size, compressed | 3 MB | 10 MB |
| CPU time per request | 10 ms | 30 s by default, up to 5 min |
| Memory per isolate (JS heap and wasm memory together) | 128 MB | 128 MB |

Time spent waiting on the network doesn't count as CPU. These limits drive three choices: the
module is instantiated once per isolate, not per request (section 3); output is release builds
with dead code removed (section 7); and the size is checked in CI (section 8).

## 1. Host imports declared in Velt (#1735)

A Velt module can declare a function that the wasm host provides, with an ambient module whose
name has a `wasm:` prefix. TypeScript already has this syntax (`declare module "…" { … }`), and
Velt parses it for augmentation; the `wasm:` scheme marks it as a host import module instead:

```ts planned
declare module "wasm:velt_cf" {
  export function r2_get(bucket: JsRef, key: string): Promise<JsRef | null>;
  export function r2_put(bucket: JsRef, key: string, body: u8[], onlyIfAbsent: bool): Promise<bool>;
  export function req_method(req: JsRef): string;
}

import { r2_get, req_method } from "wasm:velt_cf";
```

Each declared function becomes a wasm import, `(import "velt_cf" "r2_get" (func …))`. The glue
provides the functions (section 2), and a package that declares host imports ships the matching
JS next to its sources (an open question below).

**Types across the boundary.** These are the only types allowed in a host import's signature:

| Velt | wasm | JS side |
|---|---|---|
| `i32`, `u32`, `bool` | `i32` | number |
| `f64` (`number`), `i64`/`u64` | `f64` / `i64` | number / BigInt |
| `string` | pointer + byte length (UTF-8) | the glue decodes or encodes |
| `u8[]` | pointer + length | `Uint8Array` view, copied by the glue if it keeps it |
| `JsRef` | `u32` | an index into the glue's handle table |
| `JsRef \| null` | `u32`, 0 for null | — |
| `Promise<T>` result | `u32` promise id | section 5 |

- `JsRef` is a std struct holding the handle. Its `[Symbol.dispose]()` tells the glue to free
  the slot, so JS objects (a `Request`, an R2 object, a D1 statement) live exactly as long as
  the Velt value. Velt code can't read JS values directly; it reads them through imports such
  as `req_method`.
- A string result is returned through the glue, which writes UTF-8 into memory it allocates
  with an exported `velt_alloc(len)`, and returns the pointer and length in a two-word out
  slot. The lowered call turns them into a Velt string (rt_abi.md "Strings"). `u8[]`
  results work the same way.
- Anything else in a signature is a compile error that names the parameter and lists the
  allowed types.

**Which targets.** Importing a `wasm:` module in a build for a target without a JS host
(native, `wasm32-wasip1`) is a compile error at the import, naming the target. Code that must
build for several targets selects the implementation per target with the mechanism
[#1716](https://github.com/velt-lang/velt/issues/1716) designs for libc and kernel32
declarations. For std, the existing gated functions (`__intrinsic_target`, #1330) extend with a
host gate, `__intrinsic_host("workers")`, and the frontend drops the code on the other side of
the gate before name resolution. #1716 and #1735 share one design; this note only fixes the wasm
side.

**Who may declare them.** Std and any package. The import module is part of a package's public
surface, so `velt check` lists the host modules a package imports.

## 2. Target, output and glue (#1736)

`--target cloudflare` builds for `wasm32-unknown-unknown` with the Workers host and writes a
directory instead of one file:

```text
target/velt/<stem>-worker/
  <stem>.wasm      the program (release: LLVM -Oz, dead code removed, debug info stripped)
  worker.mjs       the glue: velt_worker.mjs plus each package's host modules
  wrangler.toml    name, main = "worker.mjs", compatibility_date; written once, kept if edited
```

`velt_worker.mjs` lives next to `velt_web.mjs` and shares its host functions (`write` goes to
`console.log`/`console.error`, plus `now_ms` and `date_ms`). Workers import `.wasm` files as
compiled modules, so the glue instantiates the module once per isolate, the first time a request
arrives, and keeps the instance:

```js
import wasm from "./app.wasm";
export default {
  async fetch(request, env, ctx) {
    const rt = await instance(env);              // instantiate and run main once
    return rt.handle(request, env, ctx);         // section 3
  },
};
```

The target name is an open question below. The CLI contract (`contracts/cli.md`) and
`tooling/webassembly.md` gain the target either way.

## 3. Lifecycle: main, serve and each request

1. **First request in an isolate**: the glue instantiates the module and calls `velt_start()`.
   This runs `main` until it settles or until every task waits on the host (section 5).
2. **`serve(opts, handler)`** on this target doesn't listen. It stores the `VeltHandler` and
   resolves at once with a `Server` whose `port` is 0. `opts.port`, `opts.host` and `opts.tls`
   are ignored, so the same program runs natively and on Workers. The first such call wins, and
   a second `serve` throws `IoError` `EADDRINUSE`, as a second bind of one port would. If
   `main` finishes without calling `serve`, every request gets a 500 whose body says so, and the
   message is logged once.
3. **Each request**: the glue puts the JS `Request` in the handle table and calls
   `velt_fetch(req)`. That starts a handler task from the stored `VeltHandler`, exactly as the
   native server does per request, runs a turn and returns a response token. The glue returns
   a JS promise that resolves when the handler's task finishes and hands over its response
   (`respond(token, response)`, a host import). Requests that overlap in one isolate
   interleave on the one executor, like concurrent requests on one native worker.
4. **Background work**: a task the handler spawns can outlive the response. The glue passes
   `ctx.waitUntil()` a promise that resolves when the executor has no live tasks left, so
   Cloudflare keeps the isolate until they finish.
5. **Panics and exit**: a panic or `exit()` traps out of the module, and the instance is
   poisoned. The glue answers the request (and any others in flight) with 500, logs the panic
   message, and discards the instance; the next request instantiates a fresh one and runs
   `main` again. Module state is per isolate in any case, as in every Worker.

`Server.close()` is a no-op on this target, and `ServeInfo.remoteAddr` reads the
`CF-Connecting-IP` header.

## 4. Requests and responses, in Velt

`std/fetch/request.vlt` reads a received request through `velt_rt_http_req_*` today. On the
Workers host, std/http's per-target code (section 1's gate) uses host imports for the same
reads instead. The raw request is a `JsRef` handle in place of a runtime key:

| Read | Host import (on the JS `Request`) |
|---|---|
| `method`, `url` | `req.method`, `req.url` |
| headers | `[...req.headers]` flattened to one `string[]` of names and values |
| `text()`, `bytes()`, `json()` | `await req.arrayBuffer()` (a promise import) |
| body chunks (`req.body`) | the body stream's reader: `read()` per chunk (a promise import) |

A response is built in one call, `respond(token, status, headers: string[], body: u8[])`. The
glue makes `new Response(body, { status, headers })`. A streamed body (`BodyStream`) uses a
`TransformStream` whose writer is a `JsRef`, written from Velt through a promise import per
chunk.

So `Request.adopt`, `Response.toServer` and std/http's `serve` keep their structure, and only
the leaf calls differ per host. No Rust is involved: `server_request.rs`'s stubs stay for the
browser target, where nothing serves.

## 5. Async across host promises (#1737)

A host import that returns `Promise<T>` works like this:

1. The glue calls the JS function and keeps its promise under a new id `p`.
2. The import returns `p`, and the compiler wraps it in a promise leaf (as runtime leaves are
   wrapped today, rt_abi_async.md §1).
3. Polling the leaf before it settles registers the task's waker under `p` and returns
   `Pending`.
4. When the JS promise settles, the glue writes the result through the import's result
   convention (section 1) and calls the exported `velt_settle(p, ok)`. That marks the leaf
   ready, wakes its task and runs a turn.

A rejection becomes `IoError` with the JS error's `name` as the code and its `message`, so
`await` on a host import throws like any std I/O call.

**The executor's turn.** `velt_rt_block_on` gains a host-driven mode, used when the host is
Workers or the browser:

- run microtasks and ready tasks until neither is left;
- if a timer is due, fire it and continue; otherwise ask the host for a wake-up at the earliest
  deadline (`set_timer(ms)`, `setTimeout` in the glue, which calls `velt_turn()`);
- then **return to JS** instead of sleeping. The executor's state stays in place for the next
  entry.

`velt_start`, `velt_fetch`, `velt_settle` and `velt_turn` all run turns. In Workers, the
program ends only when the isolate does; in the browser, it ends when the root is done and
nothing waits, as now. The browser glue switches to the same protocol, so `sleep` stops
busy-waiting there.

**Outbound `fetch`** is Velt code over a promise import of the host's `fetch`. It goes in
std/fetch's per-target code next to the native `velt_rt_http_fetch_send` call; the body is read
as in section 4. `AbortSignal` maps to an `AbortController` handle.

**Ordering.** One turn drains Velt's own microtasks before it returns, so Velt's promise order
matches native builds. A host promise is observed on a later JS microtask, which is the order
Node gives a promise that a native resource resolves.

## 6. `velt:cloudflare` (#1738)

A std module written in Velt over host imports. Bindings are looked up by name, so no
reflection over an `Env` type is needed:

```ts planned
import { serve } from "velt:http";
import { D1Database, R2Bucket } from "velt:cloudflare";

async function main() {
  // In main, not module constants: a constant initialized by a call runs at every use.
  const blobs = R2Bucket.binding("BLOBS");   // env.BLOBS; throws if wrangler.toml lacks it
  const db = D1Database.binding("DB");
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const name = new URL(req.url).pathname.slice(1);
    const row = await db.prepare("SELECT hash FROM packages WHERE name = ?").bind(name).first<{ hash: string }>();
    if (row == null) {
      return new Response("not found", { status: 404 });
    }
    const obj = await blobs.get(`blobs/sha256/${row.hash}`);
    return obj == null ? new Response("gone", { status: 410 }) : new Response(await obj.bytes());
  });
}
```

- **R2**: `get`, `head`, `put` (with `onlyIf` for put-if-absent, which the registry's
  `BlobStore.put_if_absent` needs), `delete`, `list`.
- **D1**: `prepare`/`bind`, `first`/`all`/`run`, `batch`. Rows decode into a type argument the
  way `velt:sqlite` decodes them (typed rows, #1508).
- **KV**: `get`, `put`, `delete`, `list`.
- **Durable Objects**: a stub from a namespace and `fetch` to it. Writing the object itself in
  Velt (a class exported to the glue) is an open question.
- **Cache**: `caches.default` `match`/`put`/`delete`, which the registry's index purge needs.

A binding lookup reads `env` through a host import. The glue keeps the `env` of the current
request (it is the same object for every request in an isolate). Outside a request, during
`main` before the first request, `env` is the one passed to the first `fetch`.

## 7. Size and CPU

To measure first (#1739); these are the levers:

- **Size**: release builds only (`--target cloudflare` without `--release` warns), LLVM `-Oz`
  and `--gc-sections`, and no debug info. Std code a program doesn't reach is dropped by
  the linker; compiling std once (#1728) doesn't change the output size. Hello-world and a
  JSON API get size budgets, set after the first build at 1.5× what it measures. The
  build fails above 3 MB compressed with a note naming the Free plan's limit (an error unless
  `--allow-large`, for Paid accounts).
- **CPU**: instantiation and `main` run once per isolate, not per request. A per-request
  benchmark (hello-world, a JSON round trip, a D1 read) under `workerd` tracks the per-request
  CPU against the Free plan's 10 ms.

## 8. Testing

- **Node fake host** (fast, every PR touching the area): `velt_worker.mjs` runs under Node
  with a stub `env` (an in-memory R2/D1/KV), driven by goldens such as
  `tests/golden/workers/*.vlt` with request/response transcripts. This needs no Cloudflare
  tooling.
- **`workerd`** (the open-source Workers runtime) in CI for the same goldens plus `velt:cloudflare`
  against Miniflare's local bindings. It runs nightly and in the merge queue when the area
  changes.
- Size and CPU budgets as in section 7.

## Contracts this changes

Each needs sign-off; the PR that implements a part changes its contract.

- `docs/reference/modules.md` "Runtime declarations": `declare module "wasm:…"` host imports
  (#1735), with #1716.
- `contracts/rt_abi_async.md` §1 and §7: the server ABI on the Workers host, promise leaves
  over host promise ids, and the exports `velt_start`/`velt_fetch`/`velt_settle`/`velt_turn`/
  `velt_alloc`.
- `contracts/cli.md`: the target and its output directory.
- `tooling/webassembly.md`: the Workers host; the browser glue's new wake protocol.

## What is Rust and what is Velt

| Piece | Language | Moves to Velt |
|---|---|---|
| Host import lowering (frontend, codegen) | Rust (compiler) | with the compiler |
| Executor turn mode, `velt_settle`, timer wake-up | Rust (`velt_rt_wasm`) | when the executor moves to Velt; until then, kept to about 150 lines |
| Promise leaf for a host promise id | Rust (`velt_rt_wasm`) | with the executor |
| Request/response bridge, `serve` on Workers | Velt (std/http, std/fetch) | — |
| Outbound `fetch`, timers' host calls | Velt | — |
| `velt:cloudflare` | Velt | — |
| Registry core and Worker | Velt (a package) | — |
| Glue (`velt_worker.mjs`) | JS | the host's language, as small as it can be |

## Alternatives considered

- **`workers-rs` with the registry core in Rust** (#1325's first plan): proven, but it adds a
  Rust service where Velt is moving away from Rust, and it doesn't make Velt programs run on
  Workers.
- **Asyncify or JSPI** to block in host calls: unnecessary, since Velt's async functions
  are already state machines (Starting point), and Asyncify grows the module considerably.
- **`wasm32-wasip1` on Workers**: Workers' WASI support doesn't give the request, the bindings
  or sockets, so the program would still need host imports. `wasm32-unknown-unknown` is
  smaller and already has the JS glue.
- **A general JS FFI first** (call any JS method by name with dynamic values): bigger, slower
  (string dispatch per call) and untyped. Typed host imports cover std and `velt:cloudflare`;
  a dynamic layer can be built on them later if packages need it.

## Open questions

- **Target name**: `--target cloudflare`, or `--target wasm32-unknown-unknown --host workers`
  (the second leaves room for Deno Deploy, Fastly and others that share the glue).
- **Host JS for packages**: a package that declares `wasm:` imports ships `host/<module>.mjs`,
  bundled into `worker.mjs`. Is that allowed for every package, shown by `velt check`, or
  restricted like native code?
- **Durable Objects in Velt**: export a Velt class as a Durable Object (`export class Counter
  implements DurableObject`), with its storage API over host imports.
- **`env` access**: a binding-by-name API (section 6), or a typed `env<Env>()`, which needs
  checking interface fields against `wrangler.toml` at build time.
- **Per-target selection** in packages: settled with #1716.
