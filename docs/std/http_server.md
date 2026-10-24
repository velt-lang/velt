# velt:http/server

`import { serve, HttpRequest, HttpResponse, HttpServer } from "velt:http/server"`. An HTTP/1.1
server written in Velt on [std/net](net.md), built to be fast: keep-alive and pipelining, each
request parsed in place in its connection's read buffer (nothing is copied or allocated for a
request unless the handler asks for a string), and every response to the requests one read
brought written in one go. On the TechEmpower plaintext and JSON tests it is ahead of
[velt:http](http.md) (hyper) and of Rust's axum (`bench/web`, `velt-native`).

[velt:http](http.md) remains the server with HTTP/2, TLS and the fetch-style `Request` and
`Response`; this module is the lower-level one: a handler reads the request through
`HttpRequest` and writes the response through `HttpResponse`.

```ts
import { serve, HttpRequest, HttpResponse } from "velt:http/server";

async function main() {
  const server = await serve({ port: 0 }, async (req: HttpRequest, res: HttpResponse) => {
    if (req.pathIs("/hello")) {
      res.send(`hello ${req.query()}`);
    } else if (req.pathIs("/echo") && req.isMethod("POST")) {
      res.header("X-Length", `${req.body().length}`);
      res.sendBytes(req.body());
    } else {
      res.status(404);
      res.send("not found");
    }
  });
  const r = await fetch(`http://127.0.0.1:${server.port}/hello?name=ada`);
  console.log(r.status, await r.text()); // 200 hello name=ada
  server.close();
}
```

## `serve(opts, handler)`

`serve<E>(opts: HttpServeOptions, handler: (req: HttpRequest, res: HttpResponse) =>
Promise<void, E> throws E): Promise<HttpServer>`. `HttpServeOptions { port: number; host?;
maxHeadSize?; maxBodySize?; idleTimeout? }`: the default host is 127.0.0.1 and port 0 picks a free
port. One task serves each connection; requests on one connection are answered in order.

- A head (request line and headers) larger than `maxHeadSize` (64 KiB) gets 431, a body larger
  than `maxBodySize` (8 MiB) 413, input that is not a request 400, a chunked request body 501
  (not read yet); the connection then closes, after reading what the client still sends for up
  to 2 s, so it gets the response.
- A connection that waits more than `idleTimeout` milliseconds (5000, Node's `keepAliveTimeout`)
  for its next request is closed.
- A handler that throws gets a 500 (the error is printed to stderr); one that returns without
  sending gets an empty 200.

`HttpServer`: `port: number`, `close()` (stops accepting and closes the idle connections; one in
the middle of a request answers it first).

## `HttpRequest`

Valid while the handler runs: the server reads the next request into the same buffer.

- `isMethod(m: string): bool`, `pathIs(p: string): bool`: compare without making a string.
- `method()`, `path()` (the target up to `?`), `query()` (after `?`, without it), `target()`.
- `header(name): string | null` (any case; the first if repeated), `hasHeader(name)`,
  `headers(): [string, string][]`.
- `body(): u8[]` (a copy), `bodyText(): string` (UTF-8, invalid bytes as U+FFFD).
- `keepAlive: bool`: the connection stays open after the response.

## `HttpResponse`

- `status(code)` (200 by default), `header(name, value)` (a `content-type` replaces the default).
- `send(text)` (`text/plain; charset=utf-8`), `sendBytes(bytes)` (`application/octet-stream`),
  `json(text)` (`application/json`), `html(text)` (`text/html; charset=utf-8`): write the
  response, with `Server`, `Date` and `Content-Length`; one per request.
- `sent(): bool`.

## Writing fast handlers

A module-level constant whose value is not a literal (`const PATH = utf8Encode("/json")`) is
module state, which one process-wide lock guards while a task uses it: every connection's task
would then wait for the others (#1487). Compare with string literals (`req.pathIs("/json")`) or
make such values inside the handler's setup instead.
