# velt:http

`import { serve, fetch, Request, Response } from "velt:http"`. An HTTP server and client on
hyper: HTTP/1.1 keep-alive, HTTP/2 (h2c prior knowledge, or ALPN over TLS) and HTTPS (rustls).
Handlers run concurrently on every core. A listening server keeps the process alive after
`main` returns, like Node, until `server.close()`; dropping the `Server` value does not stop
it. For a walkthrough, see [Building an HTTP server](../book/http-server.md).

- `serve<E>(opts: ServeOptions { port; host?; tls?: TlsOptions { cert; key } }, handler: (req:
  Request) => Promise<Response, E>): Promise<Server>`. The default host is 127.0.0.1. With `tls`
  (PEM certificate chain and key) the server speaks HTTPS and offers HTTP/2. Like spawned tasks,
  handlers must not mutate captured variables (use `shared`). A handler that throws gets a 500
  response (`Internal Server Error`) and its error is printed to stderr.
- `Request { method; path; query; headers: Headers; body; upgrade }` (`upgrade` is an internal
  key velt:websocket uses): `path` excludes the query and
  `query` excludes the `?`. `Headers.get(name): string | null` and `has(name)` are
  case-insensitive.
- `Response.text(body, status = 200)`, `Response.json<T>(value, status = 200)`,
  `Response.html(body, status = 200)`, `Response.bytes(body: u8[], status = 200)`.
  `.header(name, value): bool` adds a header; `.setHeader(name, value): bool` replaces it (e.g.
  the default `content-type`).
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

Notes: an untrusted certificate or a TLS failure throws `IoError`. Pass handlers as
inline async arrows: a named function that takes ownership of `req` can't be used as a
function value.
