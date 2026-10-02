# velt:http

`import { serve, fetch, Request, Response, ResponseWriter } from "velt:http"`. An HTTP server and client on
hyper: HTTP/1.1 keep-alive, HTTP/2 (h2c prior knowledge, or ALPN over TLS) and HTTPS (rustls).
Handlers run concurrently on every core. A listening server keeps the process alive after
`main` returns, like Node, until `server.close()`; dropping the `Server` value does not stop
it. For a walkthrough, see [Building an HTTP server](../book/http-server.md).

- `serve<E>(opts: ServeOptions { port; host?; tls?: TlsOptions { cert; key } }, handler: (req:
  Request) => Promise<Response, E>): Promise<Server>`. The default host is 127.0.0.1. With `tls`
  (PEM certificate chain and key) the server speaks HTTPS and offers HTTP/2. Like spawned tasks,
  handlers must not mutate captured variables (use `shared`). A handler that throws gets a 500
  response (`Internal Server Error`) and its error is printed to stderr.
- `Request` (a class) with getters `method`, `path`, `query`, `headers: Headers`, `body` and
  `upgrade` (an internal key `velt:websocket` uses), plus `header(name): string | null`. Each read
  copies that property out of the runtime request, so a handler pays only for what it reads;
  `header(name)` looks up one header without copying the others (`headers` copies them all).
  `path` excludes the query and `query` excludes the `?`. `Headers.get(name): string | null`,
  `has(name)` and `header(name)` are case-insensitive. A `Request` is valid until its handler
  settles: keep its properties, not the `Request`, in anything that outlives the handler (a
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
  resolves only after that (it consumes the `Server`). Dropping the `Server` value does not stop it.
- `fetch(url, opts: FetchOptions { method?; body?; headers?: Map<string, string>; ca? }):
  Promise<FetchResponse>`. `http://` and `https://`; HTTPS trusts Mozilla's root certificates
  (compiled in) plus the PEM CAs in `ca`; HTTP/2 is used when the server offers it.
- `FetchResponse { status; headers: FetchHeaders }`: `text()`, `json<T>()`, `bytes()`. Reading
  the body consumes the response. `json` throws `IoError` or `JsonError`, so catch `Error`.

```ts
import { serve, fetch, Request, Response } from "velt:http";

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

Notes: an untrusted certificate or a TLS failure throws `IoError`. Pass handlers as
inline async arrows: a named function that takes ownership of `req` can't be used as a
function value.
