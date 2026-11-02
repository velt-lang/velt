# Design: Velt on Cloudflare Workers

Status: approved plan ([#1734](https://github.com/velt-lang/velt/issues/1734#issuecomment-6555831786));
host functions (#1735) are the first part. Sub-issues: host functions
[#1735](https://github.com/velt-lang/velt/issues/1735), the Workers build
[#1736](https://github.com/velt-lang/velt/issues/1736), async over host promises
[#1737](https://github.com/velt-lang/velt/issues/1737), the Cloudflare bindings
[#1738](https://github.com/velt-lang/velt/issues/1738), tests and budgets
[#1739](https://github.com/velt-lang/velt/issues/1739), the registry
[#1740](https://github.com/velt-lang/velt/issues/1740), `fetch` on JS hosts
[#1785](https://github.com/velt-lang/velt/issues/1785), `serve` on Workers
[#1786](https://github.com/velt-lang/velt/issues/1786), [#1787](https://github.com/velt-lang/velt/issues/1787).

A Velt program that serves HTTP with `packages/http-server` builds to a Cloudflare Worker and
runs there unchanged:

```ts planned
import { serve } from "http-server";

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    return Response.json({ path: new URL(req.url).pathname });
  });
}
```

```sh
velt build --release --host workers app.vlt   # target/velt/app-worker/: app.wasm, worker.mjs, …
cd target/velt/app-worker && npx wrangler deploy
```

The first large user is the public package registry
([#1325](https://github.com/velt-lang/velt/issues/1325)), on Workers, R2, D1 and Durable
Objects.

## Principles

- **Velt, not Rust.** The server, `fetch`'s transport and the Cloudflare bindings are Velt
  code over typed host functions, in std and in packages alike. Where Velt can't express
  something, the language gets the feature. New Rust is limited to what the wasm executor must
  do (a host-driven turn and promise leaves), until the executor itself moves to Velt.
- **Two mechanisms; everything else is ordinary Velt code**: host functions (the language's
  way to call the JS host) and conditional imports (the way std and packages choose code per
  host). No runtime registration of transports, no overridable single functions.
- **Requests arrive like connections on a listener.** On Workers, `serve` is an accept loop
  that awaits a host promise per request. No Velt functions are exported to the host, `main`
  may await before `serve`, and the code has the TCP server's shape.

## Cloudflare's limits

From Cloudflare's documentation, October 2026; re-check them before setting budgets.

| Limit | Free | Paid |
|---|---|---|
| Worker size, compressed | 3 MB | 10 MB |
| CPU time per request | 10 ms | 30 s by default, up to 5 min |
| Memory per isolate (JS heap and wasm memory together) | 128 MB | 128 MB |

Time spent waiting on the network doesn't count as CPU. So the module is instantiated once per
isolate, output is a release build with dead code removed, and CI checks the size.

## 1. Host functions (#1735)

`declare host("<module>") function name(params): R;` is a typed WebAssembly import, in the
family of `declare intrinsic`, `declare system` and `declare generated("m")` functions:

```ts planned
import { HostRef } from "velt/host";

declare host("cloudflare") function r2Get(bucket: HostRef, key: string): Promise<HostRef | null>;
```

- What crosses: integers, `f64`, `bool`, `string`, `u8[]` / `readonly u8[]` and byte views,
  `HostRef` (std's `velt/host`: a slot of the glue's table of JS values, freed when dropped),
  `HostRef | null`, `void`, and (#1737) `Promise<…>` of those. Anything else is a compile error
  naming the parameter.
- A host function is a declaration kind of its own in the AST, HIR and VIR, not an extern with
  an encoded symbol name. Each declaration of one import has the same types.
- A declaration is allowed for every target; a reachable call in a build without a JS host
  (native, `wasm32-wasip1`) is an error naming the target and the caller, as for system
  functions.
- The glue (`velt_web.mjs`, and `velt_worker.mjs` on Workers) learns each import's types from
  the module's custom section `velt.host` and wraps the embedder's JS function: it decodes
  strings, passes `u8[]` as a live `Uint8Array` and `readonly u8[]` as a copy, keeps `HostRef`
  values in its table, and rejects results that don't fit the declared type with a
  `TypeError`. The ABI is in velt_vir's `vir/host.rs` and rt_abi.md "Host values".

## 2. Async host functions and the host-driven executor (#1737)

`declare host("m") async function f(…): Promise<T>` calls the JS function at once; the import
returns an id under which the glue keeps the JS promise, and the call's promise is a std hook
awaiting the promise leaf over that id (`velt_rt_host_promise`). When the JS promise settles,
the glue calls the exported `velt_settle(id)`, which wakes the task and runs a turn; the leaf,
polled, has the glue write the outcome (`velt.host_take`) through the import's result
convention. A rejection becomes an `IoError` (the JS error's `name` and `message`).

`velt_rt_wasm`'s executor runs in turns the host drives, and nothing unwinds through the
module: `velt_main` hands `main`'s root task to the runtime (`velt_rt_host_main`, which keeps
its state on the heap) and returns. Each turn runs microtasks, ready tasks and due timers until
none is left, asks the host for a wake-up at the next deadline (`set_timer(ms)`, which calls
`velt_turn()`), and returns. When the root is done, the runtime runs the code after it (the
uncaught error, the exit code) at once; after a `main` that returned 0 the turns go on while
spawned tasks, started promises or ref'd timers remain, as native programs do. The end reaches
the host through the import `done(code)`. A Worker keeps the program open after `main` (the accept loop
awaits a host promise) and drives requests with the same turns. Velt's `async` functions are
state machines, so waiting on the host needs no stack switching (no Asyncify, no JSPI). The
browser glue uses the same protocol, so `sleep` does not busy-wait there.

## 3. Conditional imports (std and packages)

Node's `imports` with conditions choose code per host, for std and packages alike:
`"#transport": { "js": "./transport/js.vlt", "default": "./transport/tcp.vlt" }`. The build
sets the conditions (`native`, `wasi`, `js`, and the host: `workers`, `browser`), and the
compiler checks that every variant exports the same names with the same signatures (a `velt
check` mode and the gate). This replaces a loader that prefers `x.workers.vlt` files.

## 4. `fetch` on JS hosts (#1785)

`fetch` is Velt code; its transport is the conditional import `#http-transport` (std/imports.vlt):
the Velt HTTP/1.1 and HTTP/2 client over velt/net by default (natively and under WASI), and the
host's own `fetch` under `js` (browser, Node, Deno and Workers alike), so a JS host's module
links no TCP, TLS, HTTP/2 or decompression code. The seam is two names, which both variants
export with the same signatures (the variant check compares them):

- `send(o: ClientRequest): Promise<Fetched, IoError | AbortError>`: the request as `fetch` made
  it (client/client_request.vlt: method, URL, headers, the body as text or bytes, the redirect
  mode, the signal, `ca`), resolved once the response's head arrived;
- `class Fetched`: `status`, `url`, `redirected`, `statusText()`, `headers()`, `decoded()`,
  `bytes()`, `text()`, `chunk()` and `close()`. `Request`, `Response`, `Headers` and the body
  types are shared; `Response` holds a `Fetched`.

The `js` variant (client/transport/) calls the host functions of module `velt:fetch`, which the
glue provides over the host's `fetch` (rt_abi.md "Host values"). A call is one `HostRef` (the
request's `AbortController`, then its `Response` and body reader). The request is checked as the
Velt client checks it (URL, scheme, header names and values), so a bad one fails with the same
`IoError`; the host follows redirects as `redirect` says and decodes compressed bodies. The
response's body arrives piece by piece, one async host call per `chunk()` (`bytes()` and
`text()` take the rest in one call), so `res.body` streams. While the program waits on the host
for a request with an `AbortSignal`, a task races the signal's wait against the end of that wait
and aborts the `AbortController` when the signal wins; the pending `fetch` or body read then
rejects, and `fetch` throws the signal's `AbortError` (`TimeoutError` for `AbortSignal.timeout`).
Nothing waits on the signal between waits, so an unread response keeps no task alive. The host
call itself is not raced: the wasm executor loses track of a task when a started promise loses a
race inside a stream's pull (#2040). Network failures carry the code native `fetch` gives them
(the glue maps the code of the error's `cause`: errno codes as they are, a broken compressed body
`EILSEQ`, a response that isn't HTTP `EPROTO`, TLS and certificate failures `EILSEQ`, undici's
`UND_ERR_*` to `ECONNRESET`, `ETIMEDOUT`, `EINVAL`, `ENOTSUP`, anything else `UNKNOWN`), with
the host's message. Header values cross as UTF-8, as the Velt client sends and reads them.

What differs from native: a custom `ca` is an `IoError` (`ENOTSUP`; the host verifies
certificates itself); the host refuses some requests the Velt client sends (undici: the
`keep-alive`, `upgrade` and `expect` headers, `CONNECT` and `TRACE`, credentials in the URL; the
ports the Fetch standard blocks, such as 1), and browsers drop forbidden headers; a DNS failure
is `ENOTFOUND` (natively `UNKNOWN`); a streamed request body is sent whole, as natively (no streaming upload);
`res.statusText` is the host's (empty over HTTP/2 in node, then the standard text); repeated
response headers other than `set-cookie` arrive joined with ", "; `redirect: "manual"` in a
browser gives the host's opaque redirect (status 0).

## 5. The Workers build (#1736)

The contract is [contracts/workers.md](../contracts/workers.md). `velt build --host workers`
builds for `wasm32-unknown-unknown` with the condition `workers` added to the target's, and
writes a directory (`target/velt/app-worker/`): the module, the glue (velt_web.mjs's `loadVelt`
core and `velt_worker.mjs` on it), the host modules of packages, the entry `worker.mjs`, and a
`wrangler.toml` (written once, kept when edited). Packages ship the JavaScript of their host
modules through `hostModules` in `package.vlt` (a file below `src/` whose default export makes
the functions, given the Worker's `env` and `ctx`); the build checks that every module the
program calls has it, from the glue (`velt:…`) or from the one package declaring and providing
it. The glue instantiates the module once per isolate, on the first request, runs `main` once,
and keeps the instance.

Requests reach the program through the glue's host module `velt:worker`: `fetch` queues each
request, and the program takes them with the async `accept()`, reads them, and answers with
`respond` or a streamed `respondStream`. Workers allow a request's I/O only in that request's
context and run a promise's reactions in the context that made it, so the glue does each
request's I/O in a pump in that request's own handler, and settles the program's waits from
there with `Deferred` (velt_web.mjs), which runs the turn on the caller's stack rather than in a
reaction of a promise an earlier request made, whose context may be gone. While the program has
work that outlives a response (a timer, a host promise other than `accept`), the handler keeps
its context with `ctx.waitUntil` until the program has none.

## 6. `serve` on Workers (#1786, #1787)

`packages/http-server` gets a `workers` variant of `serve` (a conditional import, section 3):
an accept loop that awaits a host promise the glue settles per request, and a `RequestSource`
that streams the request body. The handler, `Request` and `Response` are the same as natively.
The glue answers each JS `Request` with the `Response` the loop hands back for it; a task that
outlives its response is kept alive with `ctx.waitUntil()`.

## 7. `packages/cloudflare` (#1738)

A package, not std: R2 (`get`, `head`, `put` with `onlyIf`, `delete`, `list`), D1 (`prepare`,
`bind`, `first`/`all`/`run`, `batch`, typed rows through `velt/db`), KV, the cache, and Durable
Object stubs (`stub.fetch(req)`). Bindings are looked up by name on the `env` the glue keeps.

## 8. Size, CPU and testing (#1739)

- Size: release builds only, LLVM `-Oz`, dead code removed, no debug info; budgets for
  hello-world and a JSON API, and an error above the Free plan's 3 MB unless asked for.
- CPU: instantiation and `main` run once per isolate; a per-request benchmark under `workerd`
  tracks CPU against the Free plan's 10 ms.
- Tests: node with a fake host for every pull request in the area, and `workerd` (with
  Miniflare's local bindings) nightly and in the merge queue.

## Pull requests

| # | Layer | Pull request | Needs | Closes |
|---|---|---|---|---|
| 0 | bug fix | `velt_http` reads chunked response bodies | — | — |
| 1 | lang | host functions, `HostRef`, what crosses, the reachable-call error, fmt and LSP; synchronous calls | — | #1735 |
| 2 | runtime | the host-driven executor, `velt_settle`/`velt_turn`, `set_timer`, `done`; `async` host functions; no busy-waiting `sleep` in the browser | 1 | #1737 |
| 3 | lang/loader | conditional imports, with the check that variants export the same API | — | — |
| 4 | std | `fetch` on JS hosts | 2, 3 | #1785 |
| 5 | tooling | `velt build --host workers`; packages ship JS host modules | 2 | #1736 |
| 6 | package | `packages/http-server`'s `workers` variant of `serve` | 3, 5 | #1786, #1787 |
| 7 | package | `packages/cloudflare` | 5 | #1738 |
| 8 | app | the registry Worker | 6, 7 | #1740 |
| 9 | CI | `workerd` in CI, size and CPU budgets | 6 | #1739 |

## Contracts this changes

Each part changes its contract in its own pull request: the AST, HIR and VIR (host functions),
rt_abi.md and rt_abi_async.md (host values, the executor's exports), `contracts/cli.md`
(`--host`), `contracts/manifest.md` (`hostModules`), `contracts/workers.md` (the Worker glue and
`velt:worker`), and the loader's module resolution (conditional imports).

## The long-lived Worker

Settled in #1736:

- **A JS exception that unwinds through wasm** (a panic, a trap, `process.exit`, a host
  function that throws, a glue `TypeError`) leaves the instance inconsistent: the shadow stack
  pointer isn't restored and nothing is dropped. The Worker discards the instance, answers the
  requests in flight with 500, logs why, and instantiates a fresh one on the next request. An
  asynchronous host function's rejection is no such exception: it becomes an `IoError`.
- **`HostRef` slots** are used again: a released slot's index goes on a free list, and the slot
  carries a generation above the index, so slots never wrap into a value in use and a stale
  `HostRef` misses (until its index was reused 4,096 times).
- **Glue exports and size**: the five `velt_host_*` exports cost little against the budgets: a
  release hello-world Worker is 58,550 bytes, its files gzipped one by one (the module 47,050),
  and a small accept loop with a package host module 115,811 bytes; the tests' budgets are 1.5
  times these.

Still open: Durable Objects written in Velt (a class exported to the glue) rather than only
stubs.

## Alternatives considered

- **`workers-rs` with the registry core in Rust**: proven, but it adds a Rust service where Velt
  is moving away from Rust, and it doesn't make Velt programs run on Workers.
- **Asyncify or JSPI** to block in host calls: unnecessary (Velt's `async` is state machines),
  and Asyncify grows the module considerably.
- **`wasm32-wasip1` on Workers**: WASI gives neither the request nor the bindings, so the program
  would still need host functions; `wasm32-unknown-unknown` is smaller and has the JS glue.
- **A transport registered at run time**: the native TCP, TLS and HTTP/2 client would stay in
  the module (against the 3 MB limit), and code running before the registration would see the
  wrong one.
- **Overridable single functions**: std's internals would split along arbitrary lines.
- **Host imports as `declare module "wasm:<name>" { … }` with an encoded symbol name** (#1791):
  host functions are a declaration kind of their own instead, in the family of `declare
  system function`.
