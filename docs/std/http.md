# velt:http

`import { serve, ServeInfo, Server } from "velt:http"`. An HTTP server on hyper: HTTP/1.1
keep-alive, HTTP/2 (h2c prior knowledge, or ALPN over TLS) and HTTPS (rustls). Handlers take the
global [`Request`](fetch.md#request) and return the global [`Response`](fetch.md#response), the
same classes `fetch` uses, as in Deno and Bun. Handlers run concurrently on every core. A
listening server keeps the process alive after `main` returns, like Node, until
`server.close()`; dropping the `Server` value does not stop it. A `main` that fails (an uncaught
error, or a nonzero exit code) ends the process at once, servers or not. For a walkthrough, see
[Building an HTTP server](../book/http-server.md).

```ts
import { serve } from "velt:http";

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

`serve<E>(opts: ServeOptions { port; host?; tls?: TlsOptions { cert; key } }, handler: (req:
Request, info: ServeInfo) => Response | Promise<Response, E> throws E): Promise<Server>`. The default host is
127.0.0.1 (`host: "0.0.0.0"` listens on every interface); port 0 picks a free port. With `tls`
(PEM certificate chain and key) the server speaks HTTPS and offers HTTP/2. A `serve` that fails
(address in use, a TLS certificate or key that does not parse) drops the handler closure before
it throws.

- A handler returns a `Response` or a promise of one, as in Deno and Bun: an arrow or any
  function value, sync or `async` (`serve({ port: 8080 }, handle)`), and may take only `req`.
  Because requests run on several threads, a sync arrow is checked and run as an `async` one
  (its result, awaited when it is a promise, is the response), and any other function is
  called from an `async` wrapper; so every handler follows the rules below. `E` is what it
  throws (or its promise rejects with).
- Like spawned tasks, handlers, sync or `async` (and the async closures they reach), must not
  modify captured variables (use `shared`): `(req) => { count++; … }` is a compile error,
  however a sync handler gets to `serve` (a variable, a field, a function's result, a wrapper of
  `serve`). Requests run on several threads at once and each sees its own copy of what the
  handler captured: changing an object's fields through a captured variable changes that copy,
  not the caller's object (unlike Node, which shares it; #854). A captured resource
  (`[Symbol.dispose]`) needs a `clone()`, or capture it as `shared(new Mutex(…))`
  ([Async](../reference/async.md#thread-safety)).
- That includes a variable assigned by a sync callback the handler calls (or by a closure that
  callback makes): a function value stored in something it captured (`i.onChange("x")` with
  `i.onChange = (v) => { last = v; }`), in an array (`cbs[0]()`), a `Map`, a generic box or a
  `T | null` field, called by a method (`this.cb()`), passed to `setTimeout`, or returned by a
  function (`const next = makeCounter()`). Requests would all assign that one variable at the
  same time, so it is an error that names the call and the variable, and shows the variable's
  declaration rewritten to hold one value every request shares, as Node does:

  ```
  error: this handler calls `i.onChange`, which changes `last`; requests run at the same time
    = note: fix: const last = shared(new Mutex<{ value: string }>({ value: "" }))  // one value shared by every request, as in Node
    = note: then read it with `last.with((v) => v.value)` and change it with `last.with((v) => { v.value = … })`
  ```

  A 64-bit integer goes in `shared(0)` itself (`.add(1)`, `.get()`, `.set(v)`), a `number` or
  `boolean` in `shared(new Mutex(0))`. Callbacks that only read what they captured, functions
  the handler calls that change only their own variables, and closures a request makes for
  itself are not affected. Sharing such a variable without `shared(...)` is planned (#885).
  Changing an object's fields or elements through such a callback (`log.push(v)`) is not an
  error, but changes each request's copy (#854).

  Callbacks stored in the heap are followed by type: one stored in an object the handler
  reaches, or of the type of one it calls, counts as reached, unless it is only stored in fields
  that no code a request may run reads and that no code copies out (`w.onClick = …` while the
  handler reads only `w.name`; calling `w.onClick()` in `main` is fine, `const h = w.onClick` is
  not). The check errs on the side of rejecting, since a missed case can crash: a callback of a
  type the handler calls is an error even when the handler never gets to it. That includes one
  stored in another object of the same class (`other.onChange = …` while the handler calls
  `i.onChange`), in another emitter (`startup.on(() => { ready = true; })` while the handler
  calls `requests.emit(…)` on an `Emitter` of the same class), one in an array only `main` uses
  (`steps.push(() => { done++; })` while the handler reaches any `() => void` value), one stored
  by a function into an object passed to it (`setup(input)`), and one in a field any method
  reads, even a method no request calls (`fire() { this.onChange("x"); }`). Share the variable
  with `shared(...)` as the fix shows; the program then runs as in Node.

  ```ts
  import { serve } from "velt:http";

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
- A handler that throws gets a 500 response (`Internal Server Error`) and its error is printed to
  stderr, as in Deno and Bun. So does a response with an invalid header name or value.

## The request

The handler's `req` is a [`Request`](fetch.md#request) that reads the server's request lazily: a
handler pays only for what it reads.

- `req.url` is absolute, as in Deno and Bun: `http://` (`https://` over TLS), the `host` header
  (HTTP/2: `:authority`), then the path and query. Route with `new URL(req.url).pathname` and
  read the query with `url.searchParams`. `req.method` is as received (`GET`, `POST`, …).
- `req.headers` is immutable, as in Deno; `req.headers.get(name)` and `has(name)` look one
  header up (case-insensitively) without copying the others.
- The body is received while the handler reads it, not before the handler starts: `await
  req.text()` (invalid UTF-8 becomes U+FFFD), `await req.json<T>()`, `await req.bytes()`, or
  chunk by chunk with `for await (const chunk of body)` over `req.body` (a
  [`BodyStream`](fetch.md#bodystream), `null` for a request without a body), so an upload need
  not be held in memory. A body is read once; a read that fails (the client went away) throws
  `IoError`.
- A `Request` is valid until its handler settles: what the handler read stays readable, but
  reading something new afterwards (in a streamed body or a spawned task) stops the program
  with a clear error. Read what outlives the handler first (`const url = req.url`).

`info: ServeInfo` holds what the server knows beyond the request (Deno's `ServeHandlerInfo`):
`info.remoteAddr` is the client's `NetAddr { transport: "tcp", hostname, port }`.

## The response

The handler returns any [`Response`](fetch.md#response): `new Response(body, { status,
statusText, headers })` or `Response.json(value, init)`, `Response.redirect(url, status)`, or a
response `fetch` returned (a proxy: its body is passed on as it arrives).

- A string body is sent without a copy, with `content-type: text/plain;charset=UTF-8` unless
  the headers set one; `Response.json` sends `application/json`, bytes (`u8[]`) send no
  `content-type`. Set any other header with `res.headers.set(name, value)` or in `init`.
- A body made while it is sent (server-side rendering, large exports) is a `BodyStream`:
  `new Response(BodyStream.from(chunks))`, where `chunks` is an async generator of `u8[]`
  (JS: `ReadableStream.from`). The status and headers go out when the handler returns; each
  chunk is sent as the generator yields it (HTTP/1.1 chunked transfer, no `content-length`),
  and the generator is not asked for the next one before the client took the previous one
  (backpressure). If the client goes away, the generator is closed (its `finally` blocks run);
  if it throws, the error is printed to stderr and the response is cut off, so the client sees
  a failed body rather than a complete one.
- A 204, 205 or 304 status has no body: `new Response(body, { status: 204 })` with a body stops
  the program (JS throws `TypeError`). A passed-on fetched response with a 1xx, 204 or 304
  status sends none.

```ts
import { serve } from "velt:http";
import { BodyStream } from "velt:fetch";
import { utf8Encode } from "velt:encoding";

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

## `Server`

`Server { port }`: `close()` stops accepting, lets in-flight requests finish and closes idle
connections; once the last request finished the handler closure is dropped, so values it
captured are released (their `[Symbol.dispose]()` runs). `await server.shutdown()` does the same
and resolves only after that (it consumes the `Server`); a program that exits right after
`close()` may exit before the handler is dropped. Dropping the `Server` value does not stop it.
Servers are built only by `serve`: `new Server()` holds no runtime object (its `port` is 0 and
`close()` does nothing).

## Migrating from the server's own `Request` and `Response`

Before #638, `velt:http` had a `Request`, `Response` and `ResponseWriter` of its own. Handlers
now take and return the globals; importing the old names is an error that says so.

| Before | Now |
|---|---|
| `import { serve, Request, Response } from "velt:http"` | `import { serve } from "velt:http"` (`Request` and `Response` are global) |
| `req.path`, `req.query` | `new URL(req.url).pathname`, `url.search.slice(1)` or `url.searchParams` |
| `req.body` (a string) | `await req.text()` (or `req.json<T>()`, `req.bytes()`, `req.body` as a stream) |
| `req.header(name)` | `req.headers.get(name)` |
| `Response.text(body, status)` | `new Response(body, { status })` (`content-type: text/plain;charset=UTF-8`) |
| `Response.json(value, status)` | `Response.json(value, { status })` |
| `Response.html(body)` | `new Response(body, { headers: { "content-type": "text/html; charset=utf-8" } })` |
| `Response.bytes(data)` | `new Response(data)` (no default `content-type`) |
| `res.header(name, value)`, `res.setHeader(name, value)` | `res.headers.append(name, value)`, `res.headers.set(name, value)` |
| `Response.stream(async (w) => { w.write(…); await w.flush(); })` | `new Response(BodyStream.from(gen()))` with an async generator yielding `u8[]` chunks |
| `Response.text("", 204)` | `new Response(null, { status: 204 })` |
| `renderToStream(el, w)` (`velt:jsx`) | `new Response(renderToStream(el), init)` |
