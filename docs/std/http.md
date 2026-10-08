# velt:http

`import { serve, Request, Response, ResponseWriter } from "velt:http"`. An HTTP server on hyper
(the client is the global [`fetch`](fetch.md)): HTTP/1.1 keep-alive, HTTP/2 (h2c prior knowledge, or ALPN over TLS) and HTTPS (rustls).
Handlers run concurrently on every core. A listening server keeps the process alive after
`main` returns, like Node, until `server.close()`; dropping the `Server` value does not stop
it. A `main` that fails (an uncaught error, or a nonzero exit code) ends the process at once,
servers or not. For a walkthrough, see [Building an HTTP server](../book/http-server.md).

- `serve<E>(opts: ServeOptions { port; host?; tls?: TlsOptions { cert; key } }, handler: (req:
  Request) => Promise<Response, E>): Promise<Server>`. The default host is 127.0.0.1. With `tls`
  (PEM certificate chain and key) the server speaks HTTPS and offers HTTP/2. Like spawned tasks,
  handlers (and async closures they reach) must not mutate captured variables (use `shared`). Requests run on several threads
  at once and each gets its own copy of what the handler captured, so a captured resource
  (`[Symbol.dispose]`) needs a `clone()`, or capture it as `shared(new Mutex(…))`
  ([Async](../reference/async.md#thread-safety)). A handler that throws gets a 500
  response (`Internal Server Error`) and its error is printed to stderr.
- `Request` (a class) with getters `method`, `path`, `query`, `headers` (the global
  [`Headers`](fetch.md#headers)), `body` and `upgrade` (an internal key `velt:websocket` uses),
  plus `header(name): string | null`. Each read copies that property out of the runtime
  request, so a handler pays only for what it reads; `header(name)` looks up one header without
  copying the others (`headers` copies them all). `path` excludes the query and `query`
  excludes the `?`. `headers.get(name)`, `headers.has(name)` and `header(name)` are
  case-insensitive. A `Request` is valid until its handler settles: keep its properties, not the `Request`, in anything that outlives the handler (a
  `Response.stream` body, a spawned task). Reading a released `Request` stops the program with a
  clear error.
- `Response.text(body, status = 200)`, `Response.json<T>(value, status = 200)`,
  `Response.html(body, status = 200)`, `Response.bytes(body: u8[], status = 200)`.
  `.header(name, value): bool` adds a header; `.setHeader(name, value): bool` replaces it (e.g.
  the default `content-type`). A 1xx, 204 (`Response.text("", 204)`) or 304 status has no body:
  the body argument is dropped and no `content-type` is added (with `Response.stream`, the
  writer's writes return `false`).
- `Response.stream<E>(body: (w: ResponseWriter) => Promise<void, E>, status = 200)`: a body
  produced while it is sent (server-side rendering, large exports). `body` starts at once and
  keeps running after the handler returned the response; the status and headers (set them
  before returning; default `content-type: text/plain; charset=utf-8`) go out first, then every
  flushed chunk (HTTP/1.1 chunked transfer, no `content-length`). The response ends when `body`
  returns; if it throws, the error is printed to stderr and the response is cut off (the client
  sees a failed body, not a complete one). `ResponseWriter` (a handle):
  `write(text): bool` / `writeBytes(data): bool` buffer, `await flush(): bool` sends the buffer
  and waits while the client is behind (backpressure), `await close(): bool` ends the response
  early, `abort()` cuts it off. Once the client has gone away they return `false` and discard
  their data, so a long-running producer should stop when they do.
- `Server { port }`: `close()` stops accepting, lets in-flight requests finish and closes idle
  connections; once the last request finished the handler closure is dropped, so values it
  captured are released (their `[Symbol.dispose]()` runs). `await server.shutdown()` does the same and
  resolves only after that (it consumes the `Server`); a program that exits right after
  `close()` may exit before the handler is dropped. Dropping the `Server` value does not stop
  it. A `serve` that fails (address in use, a TLS certificate or key that does not parse)
  drops the handler closure before it throws.
- Requests, responses and servers are built only by this module (`Response.text` and the
  other constructors, `serve`); their runtime handles are private and checked by the runtime,
  so a stale one never reaches freed memory. `new Response()`, `new Request()` and `new
  Server()` compile but hold no runtime object: a handler that returns such a response answers
  500, the request's accessors stop the program (as for a released one), and the server's
  `port` is 0 and `close()` does nothing.
- This module's `Request` and `Response` are the server's and differ from the global ones of
  [`fetch`](fetch.md); importing them hides the global names in that module, so a module that
  also builds fetch requests imports them under other names (`import { Request as
  ServerRequest } from "velt:http"`). **Planned**: one `Request` and `Response` for both, as
  in Deno and Bun.

```ts
import { serve, Request, Response } from "velt:http";

async function main() {
  const server = await serve({ port: 0 }, async (req: Request): Promise<Response> => {
    if (req.path == "/hello") {
      return Response.json({ greeting: `hello ${req.query}` });
    }
    return Response.text("not found", 404);
  });
  const res = await fetch(`http://127.0.0.1:${server.port}/hello?world`);
  console.log(res.status, res.headers.get("content-type"), await res.text());
  const missing = await fetch(`http://127.0.0.1:${server.port}/nope`);
  console.log(missing.status); // 404
  server.close();
}
```

```ts
import { serve, Request, Response, ResponseWriter } from "velt:http";

async function main() {
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    const res = Response.stream(async (w: ResponseWriter) => {
      w.write("<!doctype html><ul>");
      for (let i = 0; i < 3; i++) {
        w.write(`<li>${i}</li>`);
        if (!(await w.flush())) {
          return; // the client went away
        }
        await sleep(100);
      }
      w.write("</ul>");
    });
    res.setHeader("content-type", "text/html; charset=utf-8");
    return res;
  });
}
```

Notes: a handler is an async arrow or a named async function (`serve({ port: 8080 },
handle)`).
