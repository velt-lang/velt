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
velt build --release --host workers app.vlt   # app.wasm, velt_worker.mjs, wrangler.toml
npx wrangler deploy
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

`fetch` is Velt code; its `#transport` is the Velt TCP client natively and the host's `fetch`
under `js` (browser, Node, Deno and Workers alike). Bodies stream both ways through a host
reader and writer, and `AbortSignal` maps to an `AbortController` held as a `HostRef`.

## 5. The Workers build (#1736)

`velt build --host workers` builds for `wasm32-unknown-unknown` and writes the module,
`velt_worker.mjs` and a `wrangler.toml` (written once, kept when edited). Packages ship JS host
modules through `package.vlt`, and the build checks them against their `declare host` modules.
The glue instantiates the module once per isolate, on the first request, runs `main`, and keeps
the instance.

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
(`--host`), and the loader's module resolution (conditional imports).

## Open questions for the long-lived Worker

To settle in #1736:

- **A JS exception that unwinds through wasm** (a host function that throws, a glue
  `TypeError`) leaves the instance inconsistent: the shadow stack pointer isn't restored and
  nothing is dropped. A one-shot `runVelt` ends there; a Worker must discard the instance and
  instantiate a fresh one (and answer the requests in flight with 500), or the glue must catch
  errors in host functions and turn them into a Velt panic or a rejected promise.
- **`HostRefs.next` only grows**: in a long-lived instance it passes 2^32 and wraps on the way
  into wasm. The table needs slot reuse (a free list), with a check that a stale `HostRef`
  can't reach a reused slot (a generation in the high bits).
- **Glue exports and size**: browser modules export five runtime functions for the glue
  (`velt_host_*`); measure them against the size budget, and export them only from modules
  that import host functions if they matter.
- Durable Objects written in Velt (a class exported to the glue) rather than only stubs.

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
