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
  writes it), and `wrangler.toml` (`name`, `main = "worker.mjs"`, `compatibility_date`,
  `compatibility_flags = ["enable_request_signal"]`), written only when there is none. A build without `--release` warns.
- Host modules: every import module the program reaches (a `declare host("m")` function it
  calls; the module's `velt.host` section lists them) needs JavaScript. Modules named `velt:…`
  are the runtime glue's (`velt:worker` below). Any other comes from the package whose
  `package.vlt` lists it under `hostModules` (manifest.md), which must be a package whose own
  modules declare it; exactly one such package. Otherwise the build fails at a declaration of
  the module (`driver/host_modules.rs`): "the `workers` build has no JavaScript for host module
  `m`" (with where to list it), "package `p` provides host module `m`, which none of its modules
  declares", "host module `m` is provided by two packages". A `velt:` function must be one the
  glue has, with the same types (the table `GLUE` there): "the `workers` glue has no host module
  `velt:x`", "… has no function `f`", "`velt:worker.f` is declared with other types than the
  `workers` glue's".

## The glue

- `createWorker(module, { modules, write })` returns the Worker's handlers (`{ fetch }`).
  `modules` maps each package host module's name to its file's default export, a function
  called once per instance with `host` (`host.env`: the Worker's `env`, for bindings;
  `host.ctx`: the `ExecutionContext` of the request whose context runs the program now) that
  returns the module's
  functions. They are wrapped as velt_web.mjs wraps any host function (what crosses, the checks
  of results, async functions returning promises).
- The module is instantiated on the first request, once per isolate, and `main` runs once
  (`velt_start`); the instance serves every later request. Instantiation and `main` run in the
  first request's handler.
- The end of a program (`velt.done(code)`, reaching the glue through `loadVelt`'s `ended`): a program that
  ends with 0 before it accepted a request keeps its instance; queued and later requests get a
  500 "the program does not accept requests", logged once. Any other end (code 0 after it
  accepted, a panic's 101, a trap's 134, `process.exit(n)`, an uncaught error's 1), a host
  function that throws, or a failed instantiation discards the instance (an exception that
  unwinds through the module leaves it unusable): every request in flight gets a 500, the cause
  is logged, and the next request instantiates the module again.
- Contexts. Workers tie I/O (a request's body, a `Response`, a stream, a timer, a fetch) to
  the request whose handler made it, cancel a request whose handler waits on nothing of its
  own, and end a request's context, with what it started, once its response and its
  `ctx.waitUntil` promises are done. So each request has a pump, an async loop in its own
  handler that keeps a timer of its own pending while it waits (every 10 s) and does the
  request's I/O when the program asks: reading the body, making the `Response` (the glue's 500s
  too), writing a streamed body. The program's turns run in a request's context: `main` in the
  first request's, a request handed to `accept()` in its own (from its pump), a wait on a
  request settled from its pump (velt_web.mjs's `Deferred`, which runs the turn on the caller's
  stack instead of in a reaction of a promise another request made), a host promise's
  settlement in the context of the turn that started it (`loadVelt`'s `owner`/`resuming`), the
  program's timer in the pump holding it (`loadVelt`'s `setTimer`): the newest request not
  answered yet, else the newest request; it moves when that changes (its timeout is cleared in
  the pump that set it), and a turn asking for the same deadline keeps it. A queued request
  whose client went away (`request.signal`, which Workers abort only with the compatibility flag
  `enable_request_signal`) leaves the queue and is answered 499.
- `ctx.waitUntil(p)`: a request whose pump must go on after its response calls it with the
  pump: while host promises started in its context are pending, while it holds the program's
  timer, or while the program still writes its body. Work that outlives Cloudflare's limit for
  `waitUntil` (30 s after the response) is cancelled with the context; the timer moves to the
  next request.
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

An `accept()` whose promise the program drops after the glue handed it a request (a race it
lost) gives the request back to the next `accept()`.
