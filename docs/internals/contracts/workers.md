# Host builds — CONTRACT (#1736)

`velt build --host <package>` builds a program for the JavaScript platform a **host package**
describes (design: [cloudflare-workers.md](../design/cloudflare-workers.md)). The compiler names
no platform: a platform is a package (packages/host-cloudflare is the first), and adding one
needs no compiler change. This is the contract between the build (`crates/veltc/src/commands/
host.rs`, `driver/host_modules.rs`), host packages (`vpm::host`, manifest.md's `host`), the
glue's host-neutral server core (`crates/velt_rt_wasm/js/velt_serve.mjs`), and the Velt code
that serves through `velt:worker` (packages/http-server's `workers` variant, #1786) or binds a
platform's services (packages/cloudflare, #1738).

## The build

- `--host <package>` names a package of the build's dependency graph whose `package.vlt` has
  `host: { target, condition, entry, scaffold? }`. The build is for its `target` (one the
  JavaScript glue runs: `wasm32-unknown-unknown`; a `--target` naming another is an error) and
  adds its `condition` to the target's (`wasm`, `js`): a `#name` variant of any package under
  that condition is chosen before one under `js` when written first (`vpm::imports::conditions`).
  The host package is part of the build stamp's key.
- Output: a directory, `-o <dir>`, else `<name>-<condition>/` in the build's default directory
  (`target/velt/` of the file's package; a package's name is without its `@scope/`). It must be
  new, empty, or hold nothing but files the host package scaffolds and, when it has `.velt-host`
  (an earlier host build's marker), the build's own files (`vpm::host::BUILD_FILES`, by-products
  `program.o`, `program.entry.o` and the stamps included) and the entries and scaffolded files
  the marker lists (`entry <name>`, `scaffold <name>`, one per line). The entry replaces only a
  listed entry, never a scaffolded file. Otherwise the build fails before compiling, so it never
  writes over the user's files, whichever host an earlier build was for. Names are compared with
  the build's files without ASCII case (`vpm::host::is_build_file`; a case-insensitive file
  system takes `.VELT-HOST` for `.velt-host`), and the marker is read before the build writes
  anything. The build writes through what the directory holds: a symbolic link the user put
  there under a build file's name is followed (packages ship plain files only). It holds `program.wasm` (with the build's object and stamps), `velt_web.mjs` (the core
  glue every browser build gets), `velt_serve.mjs` (the server core below), `velt_modules.mjs`
  (generated: `export const modules = { ["<module>"]: <its default export>, … }` with computed
  keys, so `__proto__` is a key), the host modules of packages under
  `hosts/<package>/<path below src/>`, the host package's `entry` under its file name (every
  build writes these), each `scaffold` file only when absent (`{{name}}` replaced by the
  program's name: lowercase letters, digits and `-`, at most 63), and `.velt-host`. A build
  without `--release` warns.
- Host modules: every import module the program reaches (a `declare host("m")` function it
  calls; the module's `velt.host` section lists them) needs JavaScript. Modules named `velt:…`
  are the runtime glue's (`velt:worker` below, `velt:fetch` of velt_web.mjs): only std declares
  their functions (sema: "the import modules `velt:…` are the runtime glue's"), so their types
  are the glue's and the compiler keeps no table of them. Any other comes from the package whose
  `package.vlt` lists it under `hostModules` (manifest.md), which must be a package whose own
  modules declare it; exactly one such package. Otherwise the build fails at a declaration of
  the module (`driver/host_modules.rs`): "the `--host <p>` build has no JavaScript for host
  module `m`" (with where to list it), "package `q` provides host module `m`, which none of its
  modules declares", "host module `m` is provided by two packages".

## The entry: a host package's adapter

The entry is the platform's module, which imports `./program.wasm` however the platform loads a
module, `createServer` from `./velt_serve.mjs` and `modules` from `./velt_modules.mjs`, and hands
the platform's requests to the server. packages/host-cloudflare's:

```js
import module from "./program.wasm";
import { createServer } from "./velt_serve.mjs";
import { modules } from "./velt_modules.mjs";
const server = createServer(module, { modules });
export default {
  fetch: (request, env, ctx) =>
    server.serve(request, {
      context: { env, ctx },
      waitUntil: (p) => ctx.waitUntil(p),
      remoteAddr: request.headers.get("cf-connecting-ip") ?? "",
    }),
};
```

## The server core

- `createServer(module, { modules, write })` returns `{ serve(request, platform) }`. `serve`
  takes a Fetch `Request` and answers with a promise of a `Response`. `platform.context` is the
  request's context as the platform has it; `platform.waitUntil(p)` (optional) keeps the
  request's handler alive after its answer, for platforms that end it with the answer.
  `platform.remoteAddr` (optional) is the client's address as the platform gives it (a host name
  or `host:port`; host-cloudflare's: `cf-connecting-ip`), which `velt:worker`.`remoteAddr` reports.
  `modules` maps each package host module's name to its file's default export, a function
  called once per instance with `host`, whose properties are those of the `context` of the
  request whose context runs the program now (packages/host-cloudflare's: `host.env`,
  `host.ctx`), and that returns the module's functions. Modules are made in the first request's
  context; a host function reads `host.*` when it is called (after an `await` inside it, the
  context may be another request's). They are wrapped as velt_web.mjs wraps
  any host function (what crosses, the checks of results, async functions returning promises).
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
- Contexts. Some platforms (Cloudflare's Workers) tie I/O (a request's body, a `Response`, a stream, a timer, a fetch) to
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
  `enable_request_signal`, set by host-cloudflare's scaffolded `wrangler.toml`) leaves the
  queue and is answered 499.
- A pump runs its jobs in order without waiting for their I/O (a read waiting for the client, a
  write under backpressure): it lives while one is in flight, and a timer it holds or an answer
  is never held up by it. A `fetch` call's body is read and cancelled in the pump of the request
  that started it (velt_web.mjs's `fetchHost` with `loadVelt`'s `context`/`within`), unless that
  pump has ended.
- `platform.waitUntil(p)`: a request whose pump must go on after its response calls it with
  the pump: while host promises started in its context are pending, while it holds the program's
  timer, or while the program still writes its body. Work that outlives Cloudflare's limit for
  `waitUntil` (on Workers, 30 s after the response) is cancelled with the context; the timer
  moves to the next request.
- `HostRef` slots of a long-lived instance: velt_web.mjs's `HostRefs` uses a released slot's
  index again with the next generation (12 bits) above the index (20 bits: at most 1,048,575
  values held at once), so slots never wrap into a value in use and a stale slot misses.

## Requests: the host module `velt:worker`

Requests arrive like connections on a listener. `serve(request, platform)` puts the request in
a queue; the program takes requests with `accept()`, in the order they came, and answers each.
`main` may await anything before it accepts; a request waits in the queue until then. Std
declares the functions, in `velt/host/worker` (std/host/worker/requests.vlt), and programs
import them from there (packages/http-server's `workers` variant will):

```ts ignore
declare host("velt:worker") async function accept(): Promise<HostRef>;
declare host("velt:worker") function method(req: HostRef): string;
declare host("velt:worker") function url(req: HostRef): string;
declare host("velt:worker") function headers(req: HostRef): string;
declare host("velt:worker") function remoteAddr(req: HostRef): string;
declare host("velt:worker") function hasBody(req: HostRef): bool;
declare host("velt:worker") async function read(req: HostRef): Promise<u8[]>;
declare host("velt:worker") function respond(req: HostRef, status: u16, headers: string, body: readonly u8[]): void;
declare host("velt:worker") function respondStream(req: HostRef, status: u16, headers: string): HostRef;
declare host("velt:worker") async function write(stream: HostRef, chunk: readonly u8[]): Promise<void>;
declare host("velt:worker") async function close(stream: HostRef): Promise<void>;
```

- `accept()` settles with the next request (a `HostRef` of the glue's request).
- `method`, `url`: the request's. `headers`: its header fields as `name: value` lines joined by
  `\n` (names lowercase, as `Headers` iterates them; `set-cookie` once per value).
- `remoteAddr(req)`: the client's address, `host:port` (`[v6]:port`; port 0 when the platform
  gave none), empty when the adapter passed none. `hasBody(req)`: whether the request has a body
  (the `Request`'s `body` is not null).
- `read(req)`: the next piece of the body, never empty; empty at its end (and for a request
  without a body, or once its answer is complete). A rejection is the stream's error.
- `respond(req, status, headers, body)`: the answer with a whole body (`headers` in the same
  `name: value` form; an empty body is none). A status or header the `Response` constructor
  rejects answers 500 instead and logs why.
- `respondStream(req, status, headers)`: the answer with a body the program writes: `write`
  settles once the chunk was taken (backpressure), `close` once the body ended; both reject when
  the client went away. Dropping the stream's `HostRef` without `close` aborts the body.
- A request is answered once: a second `respond`/`respondStream` is logged and ignored (the
  stream it returns rejects every `write`), so the instance and other requests go on. A request whose last `HostRef` the program drops unanswered gets a 500, logged.

An `accept()` whose promise the program drops after the glue handed it a request (a race it
lost) gives the request back to the next `accept()`.
