# Cloudflare Workers — CONTRACT (#1736)

`velt build --host workers` makes a Cloudflare Worker of a program (design:
[cloudflare-workers.md](../design/cloudflare-workers.md)). This is the contract between the
Worker glue (`crates/velt_rt_wasm/js/velt_worker.mjs`), the build (`crates/veltc/src/commands/
workers.rs`, `driver/host_modules.rs`), and the Velt code that serves on Workers
(`packages/http-server`'s `workers` variant, #1786) or binds Cloudflare's services
(`packages/cloudflare`, #1738).

## The build

- `--host workers` builds for `wasm32-unknown-unknown` (a `--target` naming another triple is an
  error) and adds the condition `workers` to the target's (`wasm`, `js`): a `#name` variant
  under `workers` is chosen before one under `js` when written first (`vpm::imports::HOSTS`,
  `vpm::imports::conditions`). The host is part of the build stamp's key.
- Output: a directory, `-o <dir>`, else `<stem>-worker/` in the build's default directory
  (`target/velt/` of the file's package, or `./target/velt/`; a package's is
  `target/velt/<name>-worker/`). It holds `<stem>.wasm` (with the build's object and stamps),
  `velt_web.mjs` (the core glue every browser build gets), `velt_worker.mjs`, the host modules of
  packages under `hosts/<package>/<path below src/>`, `worker.mjs` (the entry; every build
  writes it), and `wrangler.toml` (`name`, `main = "worker.mjs"`, `compatibility_date`), written
  only when there is none. A build without `--release` warns.
- Host modules: every import module the program reaches (a `declare host("m")` function it
  calls; the module's `velt.host` section lists them) needs JavaScript. Modules named `velt:…`
  are the runtime glue's (`velt:worker` below). Any other comes from the package whose
  `package.vlt` lists it under `hostModules` (manifest.md), which must be a package whose own
  modules declare it; exactly one such package. Otherwise the build fails at a declaration of
  the module (`driver/host_modules.rs`): "the `workers` build has no JavaScript for host module
  `m`" (with where to list it), "package `p` provides host module `m`, which none of its modules
  declares", "host module `m` is provided by two packages".

## The glue

- `createWorker(module, { modules, write })` returns the Worker's handlers (`{ fetch }`).
  `modules` maps each package host module's name to its file's default export, a function
  called once per instance with `host` (`host.env`: the Worker's `env` of the latest request,
  for bindings; `host.ctx`: that request's `ExecutionContext`) that returns the module's
  functions. They are wrapped as velt_web.mjs wraps any host function (what crosses, the checks
  of results, async functions returning promises).
- The module is instantiated on the first request, once per isolate, and `main` runs once
  (`velt_start`); the instance serves every later request. Instantiation and `main` run in the
  first request's handler.
- The end of a program (`velt.done(code)`, velt_web.mjs `loadVelt`'s `done`): with code 0 the
  instance stays; queued and later requests get a 500 "the program does not accept requests",
  logged once, and requests the program took but had not answered get a 500, each logged.
  Any other code (a panic is 101, a trap 134, `process.exit(n)`, an uncaught error 1), a host
  function that throws, or a failed instantiation discards the instance (an exception that
  unwinds through the module leaves it unusable): every request in flight gets a 500, the cause
  is logged, and the next request instantiates the module again.
- `ctx.waitUntil(p)`: a request's handler, once it has its response, calls it when the program
  has work left that a request does not start (a timer it asked for, a host promise other than
  `accept()`), with a promise that settles when it has none (checked after each turn), or when
  the instance ends. A response whose body the program still writes also keeps its pump alive
  with `waitUntil`.
- `HostRef` slots of a long-lived instance: velt_web.mjs's `HostRefs` uses a released slot's
  index again with the next generation (12 bits) above the index (20 bits: at most 1,048,575
  values held at once), so slots never wrap into a value in use and a stale slot misses.

## Requests: the host module `velt:worker`

Requests arrive like connections on a listener. The Worker's `fetch(request, env, ctx)` puts
the request in a queue; the program takes requests with `accept()`, in the order they came, and
answers each. `main` may await anything before it accepts; a request waits in the queue until
then. The program declares the functions it uses (`declare host("velt:worker") …`, as
packages/http-server's `workers` variant will):

```ts ignore
declare host("velt:worker") async function accept(): Promise<HostRef>;
declare host("velt:worker") function method(req: HostRef): string;
declare host("velt:worker") function url(req: HostRef): string;
declare host("velt:worker") function headers(req: HostRef): string;
declare host("velt:worker") async function read(req: HostRef): Promise<u8[]>;
declare host("velt:worker") function respond(req: HostRef, status: u16, headers: string, body: readonly u8[]): void;
declare host("velt:worker") function respondStream(req: HostRef, status: u16, headers: string): HostRef;
declare host("velt:worker") async function write(stream: HostRef, chunk: readonly u8[]): Promise<void>;
declare host("velt:worker") async function close(stream: HostRef): Promise<void>;
```

- `accept()` settles with the next request (a `HostRef` of the glue's request).
- `method`, `url`: the request's. `headers`: its header fields as `name: value` lines joined by
  `\n` (names lowercase, as `Headers` iterates them; `set-cookie` once per value).
- `read(req)`: the next piece of the body, never empty; empty at its end (and for a request
  without a body, or once its answer is complete). A rejection is the stream's error.
- `respond(req, status, headers, body)`: the answer with a whole body (`headers` in the same
  `name: value` form; an empty body is none). A status or header the `Response` constructor
  rejects answers 500 instead and logs why.
- `respondStream(req, status, headers)`: the answer with a body the program writes: `write`
  settles once the chunk was taken (backpressure), `close` once the body ended; both reject when
  the client went away. Dropping the stream's `HostRef` without `close` aborts the body.
- A request is answered once: a second `respond`/`respondStream` throws (and so ends the
  instance). A request whose last `HostRef` the program drops unanswered gets a 500, logged.

Workers allow a request's I/O (its body, the stream of its response) only in that request's
context, and run a promise's reactions in the context that made it. So the glue does each
request's I/O in a pump, an async loop in that request's handler, when the program asks; and it
settles the program's waits on a request (`accept`, `read`, `write`, `close`) from that pump or
handler with velt_web.mjs's `Deferred`, which runs the program's turn at once on the caller's
stack instead of in a reaction of a promise an earlier request made. (An `accept()` called while
a request already waits takes it right after the turn that called it.)
