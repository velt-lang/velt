# velt:http/server

`import { serve, HttpRequest, HttpResponse, HttpServer, HttpStream, EventStream } from
"velt:http/server"`. An HTTP/1.1 server written in Velt on [std/net](net.md), built to be fast:
keep-alive and pipelining, each request parsed in place in its connection's read buffer (nothing
is copied or allocated for a request unless the handler asks for a string), and every response
to the requests one read brought written in one go. Request and response bodies can also be
streamed, with backpressure and cancellation, for uploads, large downloads, proxies and
Server-Sent Events. On the TechEmpower plaintext and JSON tests it is ahead of
[velt:http](http.md) (hyper) and of Rust's axum (`bench/web`, `velt-native`).

It uses only public APIs: [std/net](net.md)'s `TcpStream` (`tryRead` into its own buffer,
`writeBytesLater`), SIMD vectors from [velt:simd](simd.md) to find line ends, and the byte-array
text methods of the [prelude](prelude.md) (`write`, `utf8Slice`, `utf8Equals`, themselves Velt
code on SIMD vectors). A package written in Velt can build a server, or another protocol, as fast
in the same way.

[velt:http](http.md) remains the server with HTTP/2 and the fetch-style `Request` and
`Response`; this module is the lower-level one: a handler reads the request through
`HttpRequest` and writes the response through `HttpResponse`.

```ts
import { serve, HttpRequest, HttpResponse } from "velt:http/server";

async function main() {
  const server = await serve({ port: 0 }, async (req: HttpRequest, res: HttpResponse) => {
    if (req.pathIs("/hello")) {
      res.send(`hello ${req.query()}`);
    } else if (req.pathIs("/echo") && req.isMethod("POST")) {
      const body = await req.body();
      res.header("X-Length", `${body.length}`);
      res.sendBytes(body);
    } else if (req.pathIs("/clock")) {
      const events = await res.sse();
      for (let i = 0; i < 3; i++) {
        await events.send(`${i}`, "tick");
      }
      await events.end();
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
maxHeadSize?; maxHeaders?; maxBodySize?; idleTimeout?; tls?; headersTimeout?; requestTimeout? }`:
the default
host is 127.0.0.1 and port 0 picks a free port. `idleTimeout` (5 s) is how long a connection
waits for its next request or more of one; `headersTimeout` (60 s) the most a head may take
from its first byte, and `requestTimeout` (300 s) the most a body may take from its head, however
steadily they trickle in: past either, the request gets 408 and the connection closes (Node's
limits of the same names). With `tls: { cert, key }` (PEM text, as [`listenTls`](net.md) takes them) the server
speaks HTTPS. One task serves each connection; requests on one connection are answered in order.

- A head (request line and headers) larger than `maxHeadSize` (16 KiB, Node's default) or with
  more than `maxHeaders` headers (100) gets 431 and input that is not a request 400; the
  connection then closes, after reading what the client still sends for up to 2 s, so it gets
  the response. A body larger than `maxBodySize` (8 MiB) makes `body()` throw `BodyTooLarge`
  (413 if the handler lets it through); `read()` streams a body of any size.
- Requests are framed strictly as RFC 9112 says, so that a proxy in front of the server cannot
  find other request boundaries than it does (request smuggling). The request line is a token
  method, a target without spaces or control characters, and exactly `HTTP/1.0` or `HTTP/1.1`;
  header names are tokens, values have no control characters but TAB (no NUL, no bare CR or LF),
  and no line is folded. An HTTP/1.1 request needs exactly one `Host` and no request two.
  Two `Content-Length` headers, `Transfer-Encoding` with `Content-Length`, `Transfer-Encoding` in
  HTTP/1.0, or codings that do not end in `chunked` get 400; another coding before `chunked`
  (`gzip, chunked`) gets 501. A chunked body takes CR LF line ends only, and nothing after a
  chunk size but whitespace and `;` extensions; one framed otherwise, or cut short, gets 400
  (not logged) and the connection closes.
- The handler runs once a request's head has arrived; a body it does not read is skipped before
  the connection's next request (one larger than `maxBodySize` closes the connection instead).
- A connection that waits more than `idleTimeout` milliseconds (5000, Node's `keepAliveTimeout`)
  for its next request is closed.
- A handler that throws gets a 500 (the error is printed to stderr); one that returns without
  sending gets an empty 200. A handler that throws because the client went away while its
  response was being written (a streamed one) is not logged: nothing can be sent.
- A connection with a body still to come gets the responses to the requests before it first, so
  a client that waits for them (or for `100 Continue`) sends it.
- Accepting stops only with `close()`: when it fails otherwise (out of file descriptors), the
  server waits (5 ms, doubling up to 1 s) and accepts again.

`HttpServer`: `port: number`, `close()` (stops accepting and closes the idle connections; one in
the middle of a request answers it first).

## `HttpRequest`

Valid while the handler runs: the server reads the next request into the same buffer. The body
is framed by `Content-Length` or `Transfer-Encoding: chunked` (decoded; extensions and trailers
are skipped); an HTTP/1.1 client that sent `Expect: 100-continue` and waits with its body gets
`100 Continue` when the handler first reads it, ahead of a response it wrote before reading. A
streamed response (`stream`, `sse`, `sendStream`) sends the 100 just before its head, as Node
does. If the handler answers at once and returns without reading, no 100 is sent: the response
goes out with `Connection: close`, and the body is not waited for (RFC 9110 §10.1.1). An
HTTP/1.0 request's expectation is ignored.

- `isMethod(m: string): bool`, `pathIs(p: string): bool`: compare without making a string.
- `method()`, `path()` (the target up to `?`), `query()` (after `?`, without it), `target()`.
- `header(name): string | null` (any case; the first if repeated), `hasHeader(name)`,
  `headers(): [string, string][]`.
- `isUpgrade(protocol): bool`: an HTTP/1.1 request asking to switch its connection to
  `protocol` (`Upgrade: protocol`, `upgrade` among the tokens of its `Connection` headers).
- `read(): Promise<u8[] | null>`: the next piece of the body as it arrives, null at its end.
  Nothing is kept: for uploads of any size, and proxies.
- `body(): Promise<u8[]>`, `bodyText(): Promise<string>` (UTF-8, invalid bytes as U+FFFD): the
  whole body (what `read()` has not taken), up to `maxBodySize` (`BodyTooLarge` beyond it):
  `res.send(await req.bodyText())`.
- `keepAlive: bool`: the connection stays open after the response: HTTP/1.1 unless `close` is
  among the `Connection` tokens (`keep-alive, close` closes), HTTP/1.0 with `keep-alive` (the
  response then says `Connection: keep-alive`).
- `takeUnread(): u8[]`: after `res.upgrade`, what the client sent past the request (in the new
  protocol, before the 101 reached it).

## `HttpResponse`

- `status(code)` (200 by default; outside 100–999 it throws `RangeError`, as Node; the reason
  phrase is Node's `http.STATUS_CODES`), `header(name, value)` (a `content-type` replaces the
  default). As Node, `header` throws `TypeError` for a name that is not a token or a value with
  a control character other than TAB, so a value taken from a request cannot add a header
  (`a\r\nX-Injected: 1`). Unlike Node, which throws for characters above U+00FF and sends a
  value as Latin-1, a value is sent as UTF-8. `Content-Length` and `Transfer-Encoding` are the
  server's, which frames the body itself: a handler's are not sent, except a `Content-Length` for
  the response to a `HEAD` request (the length a `GET` would get).
- `send(text)` (`text/plain; charset=utf-8`), `sendBytes(bytes)` (`application/octet-stream`),
  `json(text)` (`application/json`), `html(text)` (`text/html; charset=utf-8`): write the
  response, with `Server`, `Date` and `Content-Length`; one per request.
- `stream(type = "text/plain; charset=utf-8"): Promise<HttpStream>`: writes the head and
  the body piece by piece: `await s.write(data)` (a `string`, `u8[]` or `Uint8Array`) sends one
  chunk (`Transfer-Encoding: chunked`; to an HTTP/1.0 client the body ends with the connection).
  What the handler writes before it next waits leaves in one write (the head with the first
  pieces; [`writeLater`](net.md)), and `write` resolves at once unless more than 64 KiB are
  still to be sent, so a fast producer waits for a slow client;
  `await s.end()` ends the body (the server ends one the handler leaves open). A write to a
  client that went away throws `IoError` and aborts `signal()`.
- `sendStream(body: BodyStream, type = "text/html; charset=utf-8")`: streams a
  [`BodyStream`](fetch.md#bodystream) as the response, each chunk as it comes, for
  `renderToStream` of [velt:jsx](jsx.md): the markup above a pending async component reaches
  the client while it loads. A client that goes away ends it quietly: the source is closed, so
  rendering stops.
- `sse(): Promise<EventStream>`: Server-Sent Events (`text/event-stream`, `Cache-Control:
  no-cache`): `send(data, event?, id?)` writes an event (each line of `data` a `data:` line),
  `comment(text?)` a line clients ignore (a keep-alive), `end()`.
- `signal(): AbortSignal`: aborted when the client can no longer be written to or the server
  closes; pass it to the work done for the request (`fetch(url, { signal })`).
- `sent(): bool`.
- `upgrade(protocol, headers: [string, string][] = []): TcpStream`: answers `101 Switching
  Protocols` (after the responses to earlier requests) and hands the connection over: the
  stream is the caller's, the server no longer reads, writes or closes it. The client waits for
  the 101 before it speaks the new protocol; what it sent already is `req.takeUnread()`. Names and
  values are checked as `header` checks them. For WebSockets, `acceptWebSocket(req, res)` of
  [velt:websocket](websocket.md) does the handshake.

A `HEAD` request gets the head a `GET` would (with its `Content-Length`) and no body: what the
handler writes, streamed or not, is not sent. Responses with status 1xx, 204 or 304 have no body
either (and 204 no `Content-Length` or default `Content-Type`).

A proxy streams a body through without collecting it:

```ts
import { serve, HttpRequest, HttpResponse } from "velt:http/server";

async function main() {
  const server = await serve({ port: 0 }, async (req: HttpRequest, res: HttpResponse) => {
    const upstream = await fetch(`https://example.com${req.target()}`, { signal: res.signal() });
    const out = await res.stream(upstream.headers.get("content-type") ?? "application/octet-stream");
    for await (const chunk of upstream.body!) {
      await out.write(chunk);
    }
    await out.end();
  });
  server.close();
}
```

## Writing fast handlers

Keep module state off the request path. Module state (a module-level `let`, and a module
constant whose value is not a literal or that is bound to a variable, `const rows = ROWS`) is
guarded by one process-wide lock, and a task that touched it once takes that lock at the start
of every later poll. Here one task serves a connection for its whole life, so a handler that
touches module state once makes its connection take the lock for every request after that: the
connections then run one at a time (a TSX page reading a module-level array did 110k requests
a second; built per request, 665k). Compare paths with string literals (`req.pathIs("/json")`),
build data per request, and keep shared mutable state in a `shared(new Mutex(...))` passed to
the handler (#1487 makes constants free to read).
