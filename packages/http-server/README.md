# http-server

`import { serve, ServeInfo, HttpServer } from "http-server"`. An HTTP server written in Velt on
[velt/net](https://veltlang.com/docs/std/net/)'s TCP and TLS streams: HTTP/1.1 keep-alive and pipelining,
[HTTP/2](docs/serve-http.md#http2) (h2c with prior knowledge, or ALPN over TLS), HTTPS, response
streaming with backpressure, graceful stops (also under `velt dev`, through
[velt/dev](https://veltlang.com/docs/std/dev/)) and [WebSockets](docs/websocket.md). Handlers take the global
[`Request`](https://veltlang.com/docs/std/net/http/#request) and return the global
[`Response`](https://veltlang.com/docs/std/net/http/#response), the classes `fetch` uses, as in Deno and Bun;
they run concurrently on every core. A listening server keeps the process alive after `main`
returns, like Node, until `server.close()`; dropping the `HttpServer` value does not stop it. A
`main` that fails (an uncaught error, or a nonzero exit code) ends the process at once, servers
or not. For a walkthrough, see [Building an HTTP server](https://veltlang.com/docs/book/http-server/).

It is a package, not part of the standard library, and it uses only public APIs: what std offers
any server ([`RequestSource`, `responseParts`, `statusText`](https://veltlang.com/docs/std/net/http/#for-servers),
[velt/net/http/h2](https://veltlang.com/docs/std/net/http/#http2)'s HPACK and frames, velt/dev, velt/net/websocket's
`webSocketOver`). The lower-level API it is built on, `serveHttp` with `HttpRequest` and
`HttpResponse` (each request read in place in its connection's buffer, each response written
straight into it), is in [docs/serve-http.md](docs/serve-http.md).

```ts ignore
// package.vlt
dependencies: {
  // From the Velt repository (`velt new --template api` writes this):
  "http-server": {
    git: "https://github.com/velt-lang/velt",
    branch: "main",
    path: "packages/http-server",
  },
  // Or a checkout: "http-server": { path: "../velt/packages/http-server" },
},
```

It is written in Velt, with no native code. The full guide: https://veltlang.com/docs/packages/http-server/.

```ts
import { serve } from "http-server";

async function main() {
  const server = await serve({ port: 0 }, async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    if (url.pathname == "/hello") {
      return Response.json({ greeting: `hello ${url.searchParams.get("name") ?? "you"}` });
    }
    if (url.pathname == "/echo" && req.method == "POST") {
      return new Response(await req.text());
    }
    return new Response("not found", { status: 404 });
  });
  const res = await fetch(`http://127.0.0.1:${server.port}/hello?name=ada`);
  console.log(res.status, res.headers.get("content-type"), await res.text());
  const echo = await fetch(`http://127.0.0.1:${server.port}/echo`, { method: "POST", body: "hi" });
  console.log(await echo.text()); // hi
  server.close();
}
```

## `serve(opts, handler)`

`serve<E, F>(opts: HttpServeOptions, handler: (req: Request, info: ServeInfo) => Response |
Promise<Response, E> throws F): Promise<HttpServer, IoError>`. `HttpServeOptions` holds `port`
(0 picks a free port), `host` (default 127.0.0.1; `"0.0.0.0"` listens on every interface), `tls`
(`{ cert, key }` in PEM: the server speaks HTTPS and offers HTTP/2 with ALPN), `http2` (default
true), and the limits and timeouts of [docs/serve-http.md](docs/serve-http.md#servehttpopts-handler)
(`maxBodySize`, `idleTimeout`, …). A `serve` that fails (an address in use, a TLS certificate or
key that does not parse) throws `IoError`.

- A handler returns a `Response` or a promise of one, as in Deno and Bun: an arrow or any
  function value, sync or `async` (`serve({ port: 8080 }, handle)`), and may take only `req`.
  `E` is what its promise rejects with, `F` what it throws.
- Handlers run on several threads at once, each request with its own copy of what the handler
  captured, so a handler (and the callbacks it reaches) must not change a variable it captured:
  `(req) => { count++; … }` is a compile error at the `serve` call, with the `shared(...)` fix,
  as for any library function that runs a callback on other threads
  ([Async](https://veltlang.com/docs/reference/async/#tasks)). Neither may it change a captured
  object, in the handler itself (`c.count++`) or through what it calls: each request would change
  its own copy, where Node changes the one object, so the compiler reports such a change, naming
  the field, with the fix: share the object, `const c = shared(new Mutex(new C()))`, and change
  it with `c.with((v) => v.handle(path))`, or keep what changes in a `shared(...)` or an
  `Atomic`. Reported: a method that changes its receiver (`c.handle(req)` doing
  `this.count++`, also `async`), a function changing an object it is given, a closure the
  handler makes, a value reached from a capture (`users.get(id)!.visits++`, `find`'s result, an
  element in a `for…of` loop, an alias, an optional chain, a conditional, an `any`), a
  collection's own methods (`m.set(k, v)`, `xs.push(x)`, `BTreeMap.set`) and an interface or
  overridden method that changes its object. Reading a captured object is fine.

  Known limits (#854): the check does not see every change, and these still change only the
  request's copy, silently; use `shared(...)`, an `Atomic` or a `Mutex` for state requests
  must share:
  - an element of a spread copy (`[...list][0].visits++`, `const xs = [...list]; xs[0]…`) and
    a captured value stored in a local container first (`p.rows = list; p.rows[0].visits++`,
    `picked.push(list[0])`, `byName.set(k, u)` then `byName.get(k)!.visits++`);
  - typed arrays and other library writes into a buffer: `hist[1]++` on a captured
    `Int32Array`, `fill`, `sort`, `reverse`, `copyWithin`, `subarray().fill`, a view over a
    captured buffer (`new Uint8Array(b.buffer)`), I/O that reads into a captured buffer;
  - calls through a function value: a route table (`Map<string, handler>`), a middleware array,
    a callback parameter, even a local arrow (`const touch = (s) => { s.hits++; }; touch(c)`) or
    a function in an object (`helpers.touch(c)`) (a callback that assigns a *variable* it
    captured is reported, below);
  - a field of a new object filled from the captured value by key: through an `any`
    (`const self: any = this; self.out = this.app.plugins`), `Reflect.set(this, "out", …)`, or
    a helper or generic function that stores by key into the object it is given (also one
    called through an interface or taken as a function value), then `ctx.out.push(…)`;
  - a `RegExp` with the `g` or `y` flag used with `test` or `exec` (its `lastIndex`), and the
    caches of `Headers` and `URLSearchParams` reads, which each request keeps for itself;
  - a library `struct` (a handle such as `Channel` or `TcpStream`, whose copies share what they
    handle, but also a plain one such as `Duration`), and a chain of calls deeper than 48.

  A new object the handler makes is its own, also when it holds a captured value: `new
  Ctx(app)` then `ctx.depth++` or `ctx.out.push(s)` (a field only ever filled with new values,
  such as `out: string[] = []`), the same object handed to a callback or to an interface
  method, `{ ...cfg }` then a write, `new Map(cfg.headers)` then `set`, and the result of a
  function the compiler sees returns a new value (`makeCtx(cfg)`). A write through a part of it
  that may hold the captured value (`ctx.app.n++`, or a field some code fills from it by
  assignment, a literal or a spread) is reported, and so is an element of a copy
  (`list.filter(…)[0].visits++`, `slice`, `map`, `concat`, `toSorted`).

  It also reports some changes that are safe: through an element of a new object's list whose
  type could hold the captured value though it does not, through a field that some other code
  fills from a value that is not new, to the result of an overridden or interface method or of
  a factory that keeps what it returns, and through an interface or overridden method called on
  a new object when one of the methods the call may run changes what that object holds.

  A captured resource (`[Symbol.dispose]`) needs a `clone()`, or capture it as
  `shared(new Mutex(…))`.
- That includes a variable assigned by a sync callback the handler calls: a function value
  stored in something it captured (`i.onChange("x")` with `i.onChange = (v) => { last = v; }`),
  in an array, a `Map`, a generic box or a `T | null` field, called by a method, passed to
  `setTimeout`, or returned by a function. Requests would all assign that one variable at the
  same time, so it is an error that names the call and the variable, and shows the variable
  rewritten to hold one value every request shares, as Node does:

  ```
  error: this code runs on another thread and calls `i.onChange`, which changes `last`
    = note: fix: const last = shared(new Mutex<{ value: string }>({ value: "" }))  // one value every thread shares, as in Node
    = note: then read it with `last.with((v) => v.value)` and change it with `last.with((v) => { v.value = … })`
  ```

  Callbacks stored in the heap are followed by type, and the check errs on the side of
  rejecting, since a missed case would crash: a callback of a type the handler calls is an
  error even when the handler never gets to it. Share the variable with `shared(...)` as the fix
  shows; the program then runs as in Node.

  ```ts
  import { serve } from "http-server";

  class Input {
    onChange: (v: string) => void = (v) => {};
  }

  async function main() {
    const last = shared(new Mutex<{ value: string }>({ value: "" }));
    const i = new Input();
    i.onChange = (v: string) => {
      last.with((s) => {
        s.value = `${v}${s.value.length}`;
      });
    };
    const server = await serve({ port: 0 }, async (req) => {
      i.onChange("x");
      return new Response("ok");
    });
    await server.shutdown();
  }
  ```
- A handler that throws, or whose promise rejects, gets a 500 response (`Internal Server Error`)
  and its error is printed to stderr, as in Deno and Bun. So does a response with an invalid
  header name or value.
- A handler that panics (an index out of bounds, `panic(msg)`) gets a 500 too, and the server
  keeps serving; the panic's message is printed to stderr. Over HTTP/2 only its stream fails. The
  handler's work is abandoned, not undone (`catchPanic` in `velt/task`): what it held is leaked,
  a `Mutex` it held stays locked, and an object it was changing keeps the change half made. A
  response already streaming when the panic happens can't become a 500: its connection (or
  HTTP/2 stream) is closed. A panic in a task the handler `spawn`s still ends the process.

## The request

The handler's `req` is a [`Request`](https://veltlang.com/docs/std/net/http/#request) that reads the server's
request lazily: a handler pays only for what it reads.

- `req.url` is absolute, as in Deno and Bun: `http://` (`https://` over TLS), the `host` header
  (HTTP/2: `:authority`), then the path and query. Route with `new URL(req.url).pathname` and
  read the query with `url.searchParams`. `req.method` is as received (`GET`, `POST`, …).
- `req.headers` is immutable, as in Deno; `req.headers.get(name)` and `has(name)` look one
  header up (case-insensitively) without copying the others.
- The body is received while the handler reads it, not before the handler starts: `await req.text()`
  (invalid UTF-8 becomes U+FFFD), `await req.json<T>()`, `await req.bytes()`, or chunk by chunk with
  `for await (const chunk of body)` over `req.body` (a
  [`BodyStream`](https://veltlang.com/docs/std/net/http/#bodystream), `null` for a request without a body), so an
  upload need not be held in memory. A body is read once; a read that fails (the client went away)
  throws `IoError`.
- `req.signal` is aborted when the client can no longer be answered, as in Deno and Bun: over
  HTTP/2 when it resets its stream, over HTTP/1.1 when writing the response to it fails, and when
  the server closes. Pass it to the work done for the request (`fetch(url, { signal: req.signal })`)
  so that work stops with the client. Take it before the handler returns if a streamed body or a
  spawned task uses it.
- A `Request` is valid until its handler settles: what the handler read stays readable, but
  reading something new afterwards (in a streamed body or a spawned task) stops the program
  with a clear error. Read what outlives the handler first (`const url = req.url`).

`info: ServeInfo` holds what the server knows beyond the request (Deno's `ServeHandlerInfo`):
`info.remoteAddr` is the client's `NetAddr { transport: "tcp", hostname, port: number }`.

## The response

The handler returns any [`Response`](https://veltlang.com/docs/std/net/http/#response): `new Response(body, {
status, statusText, headers })` or `Response.json(value, init)`, `Response.redirect(url, status)`,
or a response `fetch` returned (a proxy: its body is passed on as it arrives).

- A string body is sent without a copy, with `content-type: text/plain;charset=UTF-8` unless the
  headers set one; `Response.json` sends `application/json`, bytes (a typed array, an `ArrayBuffer`
  or a `u8[]`) send no `content-type`. Set any other header with `res.headers.set(name, value)` or
  in `init`.
- A body made while it is sent (server-side rendering, large exports) is a `BodyStream`:
  `new Response(BodyStream.from(chunks))`, where `chunks` is an async generator of `u8[]`
  (JS: `ReadableStream.from`). The status and headers go out when the handler returns; each
  chunk is sent as the generator yields it (HTTP/1.1 chunked transfer, no `content-length`,
  unless the headers set one: then the body goes out with it, as in Deno and Bun, and a
  generator that yields fewer or more bytes cuts the response off),
  and the generator is not asked for the next one before the client took the previous one
  (backpressure). If the client goes away, the generator is closed (its `finally` blocks run);
  if it throws, the error is printed to stderr and the response is cut off, so the client sees
  a failed body rather than a complete one.
- A 204, 205 or 304 status has no body: `new Response(body, { status: 204 })` with a body stops
  the program (JS throws `TypeError`). A passed-on fetched response with a 1xx, 204 or 304
  status sends none.

```ts
import { serve } from "http-server";
import { BodyStream } from "velt/net/http";
import { utf8Encode } from "velt/encoding";

async function* rows(n: i64): AsyncGenerator<u8[]> {
  yield utf8Encode("<!doctype html><ul>");
  for (let i: i64 = 0; i < n; i++) {
    yield utf8Encode(`<li>${i}</li>`);
    await sleep(100);
  }
  yield utf8Encode("</ul>");
}

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    return new Response(BodyStream.from(rows(3)), {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  });
}
```

## Files: `serveFile(req, path, options?)` and `resolveUnder(root, urlPath)`

`serveFile` answers a request with a file, as Deno's `serveFile` does:
- a weak `ETag` (size and mtime, as Express makes it) and `Last-Modified`;
- 304 for `If-None-Match` / `If-Modified-Since`, 412 for a failed `If-Match` /
  `If-Unmodified-Since`;
- one `Range` of a GET answered with 206 and `Content-Range` (if an `If-Range` matches), 416 for
  a range past the end; several ranges get the whole file;
- HEAD, 404 for no file or a directory, 405 for other methods;
- the body read as the client takes it, with its `Content-Length`.

`options` sets the `contentType` (default: from the extension, `contentType(path)`) and extra
`headers` (`Cache-Control`). The pieces are exported for other bodies: `parseRange(header,
size)`, `evaluateConditions(req, validators)` and `ifRangeMatches(req, validators)`.

`resolveUnder(root, urlPath)` maps a URL path to a file under `root`. It refuses `..`, encoded
separators, NUL and symlinks that lead out of `root`, and gives null for those and for a missing
file.

```ts
import { resolveUnder, serve, serveFile } from "http-server";

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const path = await resolveUnder("./public", new URL(req.url).pathname);
    if (path == null) {
      return new Response("Not Found", { status: 404 });
    }
    return await serveFile(req, path, { headers: [["cache-control", "max-age=60"]] });
  });
}
```

## `upgradeConnection(req, protocol, headers?)`

A handler can take over its connection for another protocol (an HTTP/1.1 `Upgrade`, RFC 9110 §7.8):
`isUpgradeRequest(req, protocol): bool` says whether the client asked for `protocol`, and
`upgradeConnection(req: Request, protocol: string, headers?: HeadersInit): ConnectionUpgrade {
stream: TcpStream; response: Response }` takes the connection. Return `response` (`101 Switching
Protocols`, with `upgrade: protocol`, `connection: Upgrade` and `headers`) from the handler; from
then on the connection is `stream`, a [`velt/net`](https://veltlang.com/docs/std/net/) byte stream (over TLS if
the server is), released with `close()`.

```ts
import { isUpgradeRequest, serve, upgradeConnection } from "http-server";
import { TcpStream } from "velt/net";

async function session(conn: TcpStream): Promise<void> {
  await conn.write("hello\n");
  const line = await conn.readString();
  await conn.write(line.toUpperCase());
  conn.close();
}

async function main() {
  const server = await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    if (!isUpgradeRequest(req, "shout")) {
      return new Response("upgrade to shout", { status: 426 });
    }
    const up = upgradeConnection(req, "shout");
    spawn(session(up.stream));
    return up.response;
  });
}
```

The stream works once the response has been sent: its first read or write waits for that, so
what the session writes before goes out right after the response, and fails with `IoError`
`ECONNABORTED` if the handler returned something else. The client must wait for the `101`
before it sends anything in the new protocol. `upgradeConnection` throws `IoError` `EINVAL` if
`req` is not an upgrade request or its connection was taken already. Only HTTP/1.1 connections
can be upgraded (HTTP/2 has no `Upgrade`; WebSockets over HTTP/2 use the extended CONNECT of
RFC 8441, [docs/websocket.md](docs/websocket.md)). `head` holds what the client sent after its
request, already in the new protocol (Node's `head`). [`http-server/websocket`](docs/websocket.md)
is built on this.

## On a JS host (`velt build --host`)

The same program serves on a JavaScript platform: on a `velt build --host <package>` build (the
first host package is packages/host-cloudflare, for Cloudflare Workers), `serve` runs over the
host's request queue (std's `velt/host/worker`) instead of a TCP listener: the JS-host variant,
src/hosted/standard.vlt, which `imports` in package.vlt picks by the built-in condition `js`, so
it serves on every host package's platform. The handler, its `Request` and
`Response` and `ServeInfo` are the same:

- `serve` returns at once and `main` may return after it; `main` may also await anything first (a
  database check, a fetch), and requests wait in the host's queue until it serves.
- A request's body streams from the host as the handler reads it (`req.body`, `req.text()`), so an
  upload is never held whole before the handler sees it. A streamed `Response` body is written
  to the host as it is produced.
- `opts` names no address there (the platform routes the requests; `server.port` is 0); of its
  limits only `maxBodySize` applies (413 when the request's `content-length` is larger, before the
  handler runs; `IoError` "EMSGSIZE" from the body's readers when it turns out larger), the rest
  and TLS are the platform's. `info.remoteAddr` is what the host package's entry gives
  (host-cloudflare: `cf-connecting-ip`, port 0).
- Headers are checked as natively (a line break in a value answers 500), and a body without a
  `content-type` gets `text/plain; charset=utf-8` (text) or `application/octet-stream` (bytes,
  streams).
- `close()` stops taking requests at once (requests stay in the host's queue); once the handlers in
  flight finished the program has nothing left to do and ends, and the host starts it again
  (`main` runs again) for the next request. `shutdown()` waits for those handlers.
- A handler that throws gets a 500, logged, as natively. Upgrades (`upgradeConnection`) and
  `serveHttp` are native only.

## `HttpServer`

`HttpServer { port: number }`: `close()` stops accepting, lets in-flight requests finish and closes
idle connections; once the last request finished the handler closure is dropped, so values it
captured are released (their `[Symbol.dispose]()` runs). `await server.shutdown()` does the same
and resolves only after that (it consumes the `HttpServer`); a program that exits right after
`close()` may exit before the handler is dropped. Dropping the `HttpServer` value does not stop it.
`serve` (or velt/net's `listen`) on the port of a server just closed works: it waits (up to 5
seconds) until the closed server has released the port. The listen backlog is the OS maximum, so a
burst of connections waits to be accepted instead of being refused (macOS caps every listen queue
at `kern.ipc.somaxconn`, 128 by default).
