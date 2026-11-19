# Velt on Cloudflare Workers

Status: partly implemented. Issue: [#1734](https://github.com/velt-lang/velt/issues/1734).

## Summary

A Velt program that serves HTTP with `packages/http-server` builds to a Cloudflare Worker and
runs there unchanged:

```ts
import { serve } from "http-server";

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    return Response.json({ path: new URL(req.url).pathname });
  });
}
```

```sh
velt add host-cloudflare
velt build --release --host host-cloudflare   # writes target/velt/<name>-workers/
cd target/velt/<name>-workers && npx wrangler deploy
```

The design rests on two language mechanisms, host functions (typed calls from Velt into the
JavaScript host) and conditional imports (code chosen per platform), plus a runtime that runs in
turns the host drives. Everything else, the Worker's request loop, `fetch` over the host's own
`fetch`, and the Cloudflare bindings (R2, D1, KV, the cache, Durable Object stubs), is ordinary
Velt code in std and packages. Cloudflare itself is a package, `host-cloudflare`; the compiler
names no platform.

## Motivation

Workers run WebAssembly next to JavaScript, close to users, with no server to operate. A Velt
program should get there without being rewritten against a platform API, and without the
compiler learning about each platform. The platform's limits shape the design. As Cloudflare
documents them:

| Limit | Free | Paid |
|---|---|---|
| Worker size, compressed | 3 MB | 10 MB |
| CPU time per request | 10 ms | 30 s by default, up to 5 min |
| Memory per isolate (JS heap and wasm memory together) | 128 MB | 128 MB |

Time spent waiting on the network does not count as CPU. So the module is instantiated once per
isolate and serves many requests, output is a release build with dead code removed, and CI checks
its size and CPU.

## Design

### Principles

- **Velt, not Rust.** The server, `fetch`'s transport and the Cloudflare bindings are Velt code
  over typed host functions, in std and in packages alike. Rust is limited to what the wasm
  executor must do: a host-driven turn and promise leaves over host promises.
- **Two mechanisms.** Host functions are the way to call the JS host; conditional imports are
  the way std and packages choose code per host. There is no runtime registration of
  transports and no overridable single function.
- **The compiler knows no platform.** A platform is a host package whose `package.velt` says
  everything the build needs: the target, the condition its builds get, its entry (the
  platform's adapter over a host-neutral server core in the runtime glue) and the configuration
  files to scaffold. Deno Deploy, Fastly or a Node or Bun server is another package, with no
  compiler change. Help text and diagnostics name no platform, and the compiler keeps no table of
  what the glue provides (std declares the `velt:` functions once).
- **Requests arrive like connections on a listener.** On a JS host, `serve` is an accept loop
  that awaits a host promise per request. No Velt functions are exported to the host, `main` may
  await before `serve`, and the code has the TCP server's shape.

### Host functions

`declare host("<module>") function name(params): R;` is a typed WebAssembly import, in the
family of `declare system` and `@intrinsic declare` functions
([Reference](https://veltlang.com/docs/reference/host-functions/)):

```ts ignore
import { HostRef } from "velt/host";

declare host("cloudflare") function takeBytes(v: HostRef): u8[];
declare host("cloudflare") async function r2Get(
  bucket: string,
  key: string,
): Promise<HostRef | null>;
```

- **What crosses.** Parameters: integers, `f64`, `bool`, `string`, `u8[]`, `readonly u8[]` (a
  view of the bytes at the call), `HostRef` and `HostRef | null`. Results: `void`, integers,
  `f64`, `bool`, `string`, `u8[]`, `HostRef` and `HostRef | null`. `HostRef` (std's `velt/host`)
  is a slot of the glue's table of JS values, released when dropped. Anything else is a compile
  error naming the parameter.
- **Async host functions.** `declare host("m") async function f(…): Promise<T>` calls the JS
  function, which returns a promise; the Velt call's promise rejects with an `IoError` carrying
  the JS error's `name` and `message`.
- **Declarations.** A host function is a declaration kind of its own in the AST, HIR and VIR,
  not an extern with an encoded symbol name. Declaring one import twice with different types is
  an error. The import module `velt` is the runtime's own, and modules named `velt:…` belong to
  the runtime glue, so only std may declare their functions.
- **Targets.** A declaration is allowed for every target. A reachable call in a build without a
  JS host (native, `wasm32-wasip1`) is an error: "host function `f` is not available on
  <target>", as for C functions a target lacks.
- **The glue.** `velt_web.mjs` (and the server core `velt_serve.mjs` of host builds) learns each
  import's types from the module's custom section `velt.host` and wraps the embedder's JS
  function: it decodes strings, passes `u8[]` as a live `Uint8Array` and `readonly u8[]` as a
  copy, keeps `HostRef` values in its table, and rejects results that do not fit the declared
  type with a `TypeError`.

### The host-driven executor

On `wasm32-unknown-unknown` the program runs in turns the host drives, and nothing unwinds
through the module. The glue calls the export `velt_start`, which hands `main`'s root task to the
runtime (`velt_rt_host_main`, which keeps its state on the heap), runs the first turn and
returns. Each turn runs microtasks, ready tasks and due timers until none is left, asks the host
for a wake-up at the next deadline (the import `set_timer(ms)`, which calls the export
`velt_turn()`), and returns. When the root is done, the runtime runs the code after it (the
uncaught error, the exit code); after a `main` that returned 0 the turns go on while spawned
tasks, started promises or ref'd timers remain, as in native programs. The end reaches the host
through the import `done(code)`.

An async host function's import returns an id under which the glue keeps the JS promise; the
call awaits a promise leaf over that id (`velt_rt_host_promise`). When the JS promise settles,
the glue calls the export `velt_settle(id)`, which wakes the task and runs a turn; the leaf,
polled, has the glue write the outcome (the import `host_take`) as std's `HostSettled<T>`. A leaf
dropped first tells the glue to forget the promise (`host_forget`).

Velt's `async` functions are state machines, so waiting on the host needs no stack switching (no
Asyncify, no JSPI). The browser glue uses the same protocol, so `sleep` does not busy-wait there.

### Conditional imports

A package's `imports` map, with conditions in the manner of Node's, chooses code per host, for
std (`std/imports.velt`) and packages alike:

```ts ignore
"#http-transport": {
  js: "./net/http/fetch/client/transport/js.velt",
  default: "./net/http/fetch/client/transport/native.velt",
},
```

A build's conditions come from its target (`native` with the OS and architecture, `wasm` with
`wasi` or `js`) and, for a `--host` build, the host package's condition (`workers` for
host-cloudflare), then `default` (`vpm::imports::conditions`). The first variant written whose
condition the build has is used. The variants of one name must export the same names with the
same signatures; `velt check`, `velt test` and the language server compare them
(`crates/velt_loader/src/variants.rs`), so a program cannot compile for one target and not for
another.

### `fetch` on JS hosts

`fetch` is Velt code; its transport is the conditional import `#http-transport`: the Velt
HTTP/1.1 and HTTP/2 client over velt/net by default (natively and under WASI), and the host's own
`fetch` under `js` (browser, Node, Deno and Workers alike), so a JS host's module links no TCP,
TLS, HTTP/2 or decompression code. The seam is two names, which both variants export with the
same signatures:

- `send(o: ClientRequest): Promise<Fetched, IoError | AbortError>`: the request as `fetch` made
  it (method, URL, headers, the body as text or bytes, the redirect mode, the signal, `ca`),
  resolved once the response's head has arrived;
- `class Fetched`: `status`, `url`, `redirected`, `statusText()`, `headers()`, `decoded()`,
  `bytes()`, `text()`, `chunk()` and `close()`. `Request`, `Response`, `Headers` and the body
  types are shared; `Response` holds a `Fetched`.

The `js` variant (`std/net/http/fetch/client/transport/`) calls the host functions of module
`velt:fetch`, which the glue provides over the host's `fetch`. A call holds one `HostRef` (the
request's `AbortController`, then its `Response` and body reader). The request is checked as the
Velt client checks it (URL, scheme, header names and values), so a bad one fails with the same
`IoError`; the host follows redirects as `redirect` says and decodes compressed bodies. The
response's body arrives piece by piece, one async host call per `chunk()` (`bytes()` and `text()`
take the rest in one call), so `res.body` streams.

While the program waits on the host for a request with an `AbortSignal`, a watcher task races
the signal against the end of that wait and aborts the `AbortController` when the signal wins;
the pending `fetch` or body read then rejects, and `fetch` throws the signal's `AbortError`
(`TimeoutError` for `AbortSignal.timeout`). The host call itself is not in the race, because the
wasm executor loses track of a task when a started promise loses a race inside a stream's pull
([#2040](https://github.com/velt-lang/velt/issues/2040)). Nothing waits on the signal between
waits, so an unread response keeps no task alive.

Network failures carry the code native `fetch` gives them: the glue maps the code of the error's
`cause` (errno codes as they are, a broken compressed body `EILSEQ`, a response that is not HTTP
`EPROTO`, TLS and certificate failures `EILSEQ`, undici's `UND_ERR_*` to `ECONNRESET`,
`ETIMEDOUT`, `EINVAL` or `ENOTSUP`, anything else `UNKNOWN`), with the host's message. Header
values cross as UTF-8, as the Velt client sends and reads them.

### Host builds and host packages

The contract is [contracts/workers.md](../contracts/workers.md). `velt build --host <package>`
resolves `<package>` to a host package among the build's dependencies; its `package.velt`'s
`host` (`vpm::host`) gives the target (`wasm32-unknown-unknown`), the condition added to the
target's, the entry and the files to scaffold:

```ts ignore
host: {
  target: "wasm32-unknown-unknown",
  condition: "workers",
  entry: "./src/worker.mjs",
  scaffold: { "wrangler.toml": "./src/wrangler.toml" },
},
```

The build writes a directory (`target/velt/<name>-<condition>/`): `program.wasm`, the glue
(`velt_web.mjs` and the server core `velt_serve.mjs` on it), `velt_modules.mjs` (the host modules
the program imports), the host modules of packages, the host package's entry (`worker.mjs`,
written by every build) and its scaffolded files (`wrangler.toml`, written once and kept when
edited). Packages ship the JavaScript of their host modules through `hostModules` in
`package.velt`: a file whose default export makes the functions, given the request's context as
the entry passes it (host-cloudflare's `env` and `ctx`). The build checks that every module the
program calls has its JavaScript, either from the glue (`velt:…`) or from the one package that
declares and provides it (`crates/veltc/src/driver/host_modules.rs`).

The glue is split in two. The **core** (`velt_serve.mjs`) is host-neutral: the `velt:worker`
request queue, the per-request pumps, crash handling and the `HostRef` table; it exports
`createServer(module, { modules }).serve(request, { context, waitUntil, remoteAddr })`. It
instantiates the module once, on the first request, runs `main` once, and keeps the instance.
The **adapter** is the host package's entry, a few lines mapping the platform's entry point
(Workers' `export default { fetch(request, env, ctx) }`, `ctx.waitUntil`,
`cf-connecting-ip`) onto `serve`.

Requests reach the program through the glue's host module `velt:worker` (declared in
`std/host/worker/`): `serve` queues each request, and the program takes them with the async
`accept()`, reads them (`method`, `url`, `headers`, `hasBody`, `remoteAddr`, the async `read`),
and answers with `respond` or a streamed `respondStream`, `write` and `close`.

Workers tie I/O and timers to the request that made them, cancel a request that waits on nothing
of its own, and end what a request started with it. One program serves many requests, so the
core gives each request a pump in its own handler that keeps a timer pending and does that
request's I/O, its `Response` included. It runs the program's turns in the context they concern
(a request handed over from its pump, a host promise's settlement where it started, the
program's timer in the newest request), and a request whose context holds the program's pending
work keeps it alive with `ctx.waitUntil`. The rules are in the contract.

### `serve` on JS hosts

`packages/http-server` has a JS-host variant of the web-standard `serve`, chosen by the
conditional import `#standard` (`js: "./src/hosted/standard.velt"`). Every `--host` build has
the `js` condition, so http-server names no platform and serves on every host package's.
`serve` spawns the accept loop over std's `velt/host/worker` in a task scope and returns an
`HttpServer` (holding a stop signal, not a `TcpListener`), so `main` may await before `serve`
and return after it: the runtime keeps the program while the loop's task waits. Each request is a
`RequestSource` (`QueuedRequest`, `src/hosted/queued.velt`) whose `read` is one `velt:worker`
`read`, so the body streams. The handler, `Request` and `Response` are the same as natively. The
response is one `respond`, or `respondStream` with a `write` per chunk. Of the server options
only `maxBodySize` applies; the platform owns the address and the other limits.

### `packages/cloudflare`

A package, not std: R2 (`get`, `head`, `put` with `onlyIf`, `delete`, `list`), D1 (`prepare`,
`bind`, `first`/`all`/`run`, `batch`), KV, the cache, and Durable Object stubs
(`stub.fetch(req)`). It is Velt over one host module, `cloudflare`
(`packages/cloudflare/src/cloudflare.mjs`, shipped through `hostModules`; its functions are
declared in `src/host.velt`), and needs no compiler or runtime support.

- **Bindings by name.** `r2Bucket("BUCKET")`, `d1Database("DB")`, `kvNamespace("KV")`,
  `durableObjectNamespace("NS")` and `caches.default` / `caches.open(name)` make wrappers holding
  the name only; each host call looks the binding up in `host.env` when it is called, so no
  wrapper holds a JavaScript object of another request's. A missing binding, or one of another
  kind, rejects with `IoError` "ENOENT".
- **What crosses.** Options, metadata and listings as JSON text (`JSON.parse<T>` into structs);
  values and bodies as `u8[]`; objects the program reads later (an R2 object's body, a response)
  as `HostRef`s, released when dropped. A Durable Object id crosses as its hex string, and a stub
  is its namespace and id; the host gets the stub again for each `fetch`.
- **D1's rows.** The host writes a result as one binary frame (meta as JSON, column names, tagged
  cells: null, i64, f64, text, blob, bool, JSON text), which `D1Rows` reads as a velt/db
  `RowSource`; rows are filled with the same fields, conversions and `RowError`s as every Velt
  driver's. A frame rather than the rows' JSON text, because JSON cannot tell a BLOB from an
  array. Parameters (`string | number | bool | null | u8[]`) cross in the same frames.
- **Errors.** Async functions reject with what Cloudflare threw (its `name` is the `IoError`'s
  code). The one synchronous function that can fail (making a Durable Object id) never throws,
  since a throw from a host function ends the instance: it records the failure, which Velt
  throws as an `IoError`.
- **Requests and responses** (the cache, stubs) cross as method, URL, `name\nvalue\n…` headers and
  a whole body, as std's `fetch` crosses them on JS hosts. Bodies are whole, except an R2
  object's, which `body` streams.

### The long-lived Worker

- **A JS exception that unwinds through wasm** (a panic, a trap, `process.exit`, a host function
  that throws, a glue `TypeError`) leaves the instance inconsistent: the shadow stack pointer is
  not restored and nothing is dropped. The core discards the instance, answers the requests in
  flight with 500, logs why, and instantiates a fresh one on the next request. An async host
  function's rejection is no such exception: it becomes an `IoError`.
- **`HostRef` slots** are reused: a released slot's index goes on a free list, and the slot
  carries a generation above the index, so a stale `HostRef` misses instead of reaching another
  value (until its index has been reused 4,096 times).

## Runtime cost

Sizes are of release builds, the Worker's files gzipped one by one, as Cloudflare's limit
measures them; the tests' budgets are 1.5 times these:

| Worker | Size |
|---|---|
| hello world (`crates/veltc/tests/workers.rs`) | 59,872 bytes |
| a small accept loop with a package host module | 117,799 bytes |
| http-server's `serve` (the test's server) | 309,826 bytes |
| `serve` with the Cloudflare bindings (`crates/veltc/tests/workerd.rs`'s `app/`) | 445,105 bytes |

CPU, measured on the release `app/` Worker under node (`crates/veltc/tests/workerd/cpu.mjs`):
from importing the Worker to its first answer, 62-78 ms on a Linux x86-64 builder (budget
250 ms, a quarter of Workers' 1 s limit for top-level code); per request, at the median, for a
text and a JSON handler, 0.17-0.27 ms (budget 2 ms, a fifth of the Free plan's 10 ms).

## Testing

- `crates/veltc/tests/workers.rs`: host builds under node with a fake host, and the size
  budgets.
- `crates/veltc/tests/cloudflare.rs`: each binding against node fakes of R2, D1, KV, `caches`
  and Durable Objects, debug and release; rejections; `HostRef`s released. The package's own
  `velt test` covers the frames natively.
- `crates/veltc/tests/workerd.rs`: uploads a release and a debug build of `app/` to workerd
  through Miniflare (pinned in `tests/workerd/`), as `wrangler deploy` uploads modules, and
  checks text, JSON, a 100 kB body, a streamed response, a handler that throws, a `fetch` of
  another service, eight requests at once, and R2, D1, KV, the cache and a Durable Object stub
  against workerd's local bindings. The Linux merge-queue job runs it (`VELT_WORKERD=1` makes
  missing packages a failure); elsewhere it is skipped without them. The nightly `workerd` job
  also runs the CPU budgets (`VELT_WORKERD_TIMINGS=1`).

## Compatibility

`fetch` on a JS host differs from native `fetch` where the host decides:

- a custom `ca` is an `IoError` (`ENOTSUP`): the host verifies certificates itself;
- the host refuses some requests the Velt client sends (undici: the `keep-alive`, `upgrade` and
  `expect` headers, `CONNECT` and `TRACE`, credentials in the URL, the ports the Fetch standard
  blocks), and browsers drop forbidden headers;
- a DNS failure is `ENOTFOUND`;
- a streamed request body is sent whole, as natively (no streaming upload);
- `res.statusText` is the host's (empty over HTTP/2 in node, then the standard text);
- repeated response headers other than `set-cookie` arrive joined with ", ";
- `redirect: "manual"` in a browser gives the host's opaque redirect (status 0).

## Alternatives considered

- **Hosts in a compiler table** (`--host workers`, the condition and `wrangler.toml` in the
  compiler, a table of the glue's functions): every platform would need a compiler change and a
  release. Hosts are packages instead.
- **Source generators for the glue table or the entry**: Velt's source generators
  ([source-generators.md](source-generators.md)) generate per-type Velt bodies from the type
  model; the entry and configuration are files a package ships, and there is no table (std
  declares the `velt:` functions, so their types are the glue's by construction).
- **`workers-rs` with server code in Rust**: proven, but it adds Rust services where Velt keeps
  its code in Velt, and it does not make Velt programs run on Workers.
- **Asyncify or JSPI** to block in host calls: unnecessary, since Velt's `async` functions are
  state machines, and Asyncify grows the module considerably.
- **`wasm32-wasip1` on Workers**: WASI gives neither the request nor the bindings, so the program
  would still need host functions; `wasm32-unknown-unknown` is smaller and has the JS glue.
- **A transport registered at run time**: the native TCP, TLS and HTTP/2 client would stay in the
  module (against the 3 MB limit), and code running before the registration would see the wrong
  one.
- **Overridable single functions**: std's internals would split along arbitrary lines.
- **Host imports as `declare module "wasm:<name>" { … }` with an encoded symbol name**: host
  functions are a declaration kind of their own instead, in the family of `declare system
  function`, so sema checks what crosses and the VIR carries the import as it is.
- **Exporting a Velt handler to the host** (the Worker's `fetch` calling into Velt per request):
  `main` could not await before serving, and the program would not have the TCP server's shape;
  an accept loop over a host promise keeps one programming model on every target.

## Limitations

- `packages/cloudflare` fills D1 rows with its own decoder rather than velt/db's `readRows`,
  whose by-column path calls runtime functions the wasm runtime lacks
  ([#2121](https://github.com/velt-lang/velt/issues/2121)); the results are the same.
- `velt run` cannot run a `--host` build.

## Not yet implemented

- **Durable Objects written in Velt**: a class exported to the glue, rather than only stubs of
  Durable Objects written elsewhere.
- **`velt check` and the editor with `--host`**: a variant that only a host package's condition
  selects is compared with the other variants, but type-checked only by
  `velt build --host <package>`.
