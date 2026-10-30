# `serveHttp`: the server's lower-level API

`import { serveHttp, HttpRequest, HttpResponse, HttpServer, HttpStream, EventStream } from
"http-server"`. The HTTP/1.1 and [HTTP/2](#http2) server [`serve`](../README.md) runs on, written in
Velt on [velt/net](../../../docs/std/net.md), built to be fast: keep-alive and pipelining, each
request parsed in place in its connection's read buffer (nothing is copied or allocated for a
request unless the handler asks for a string), and every response to the requests one read brought
written in one go. Request and response bodies can also be streamed, with backpressure and
cancellation, for uploads, large downloads, proxies and Server-Sent Events. On the TechEmpower
plaintext and JSON tests it is ahead of Rust's axum (`bench/web`, `velt-native`).

It uses only public APIs: [velt/net](../../../docs/std/net.md)'s `TcpStream` (`tryRead` into its own
buffer, `writeBytesLater`), SIMD vectors from [velt/simd](../../../docs/std/simd.md) to find line
ends, and the byte-array text methods of the [prelude](../../../docs/std/prelude.md) (`write`,
`utf8Slice`, `utf8Equals`, themselves Velt code on SIMD vectors). A package written in Velt can
build a server, or another protocol, as fast in the same way.

A handler reads the request through `HttpRequest` and writes the response through
`HttpResponse`, the same way over HTTP/1.1 and HTTP/2.

```ts
import { serveHttp, HttpRequest, HttpResponse } from "http-server";

async function main() {
  const server = await serveHttp({ port: 0 }, async (req: HttpRequest, res: HttpResponse) => {
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

## `serveHttp(opts, handler)`

`serve<E>(opts: HttpServeOptions, handler: (req: HttpRequest, res: HttpResponse) => Promise<void, E>
throws E): Promise<HttpServer>`. `HttpServeOptions { port: number; host?; maxHeadSize?; maxHeaders?;
maxBodySize?; idleTimeout?; tls?; headersTimeout?; requestTimeout?; http2?; maxConcurrentStreams? }`
(the last two: [HTTP/2](#http2)): the default host is 127.0.0.1 and port 0 picks a free port.
`idleTimeout` (5 s) is how long a connection waits for its next request or more of one;
`headersTimeout` (60 s) the most a head may take from its first byte, and `requestTimeout` (300 s)
the most a body may take from its head, however steadily they trickle in: past either, the request
gets 408 and the connection closes (Node's limits of the same names). With `tls: { cert, key }` (PEM
text, as [`listenTls`](../../../docs/std/net.md) takes them) the server speaks HTTPS. One task
serves each connection; requests on one connection are answered in order.

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
- `stream(type = "text/plain; charset=utf-8"): Promise<HttpStream>`: writes the head and the body
  piece by piece: `await s.write(data)` (a `string`, `u8[]` or `Uint8Array`) sends one chunk
  (`Transfer-Encoding: chunked`; to an HTTP/1.0 client the body ends with the connection). What the
  handler writes before it next waits leaves in one write (the head with the first pieces;
  [`writeLater`](../../../docs/std/net.md)), and `write` resolves at once unless more than 64 KiB
  are still to be sent, so a fast producer waits for a slow client; `await s.end()` ends the body
  (the server ends one the handler leaves open). A write to a client that went away throws `IoError`
  and aborts `signal()`.
- `sendStream(body: BodyStream, type = "text/html; charset=utf-8")`: streams a
  [`BodyStream`](../../../docs/std/fetch.md#bodystream) as the response, each chunk as it comes, for
  `renderToStream` of [velt/jsx](../../../docs/std/jsx.md): the markup above a pending async
  component reaches the client while it loads. A client that goes away ends it quietly: the source
  is closed, so rendering stops.
- `sse(): Promise<EventStream>`: Server-Sent Events (`text/event-stream`, `Cache-Control:
  no-cache`): `send(data, event?, id?)` writes an event (each line of `data` a `data:` line),
  `comment(text?)` a line clients ignore (a keep-alive), `end()`.
- `signal(): AbortSignal`: aborted when the client can no longer be written to or the server
  closes; pass it to the work done for the request (`fetch(url, { signal })`).
- `sent(): bool`.
- `addTrailers(trailers: [string, string][])` (Node's `res.addTrailers`): fields sent after the
  body, checked as `header` checks them; call it before the body ends. Over HTTP/1.1 they end a
  chunked body: a streamed one always, one written at once (`send` and the like) only when the
  request said `TE: trailers`, which makes the server send that body chunked; otherwise they are
  not sent (a `Content-Length` body has no place for them). Over HTTP/2 they are a HEADERS frame
  after the body, always. Name them in a `Trailer` header when the client needs to know in
  advance (gRPC's `grpc-status` is sent this way). The web `Response` has no trailers (the Fetch
  standard dropped them), so the web-standard `serve` cannot send any.
- `upgrade(protocol, headers: [string, string][] = []): TcpStream` (over HTTP/2: see
  [HTTP/2](#http2)): answers `101 Switching
  Protocols` (after the responses to earlier requests) and hands the connection over: the
  stream is the caller's, the server no longer reads, writes or closes it. The client waits for
  the 101 before it speaks the new protocol; what it sent already is `req.takeUnread()`. Names and
  values are checked as `header` checks them. For WebSockets, `acceptWebSocket(req, res)` of
  [velt/net/websocket](websocket.md) does the handshake.

A `HEAD` request gets the head a `GET` would (with its `Content-Length`) and no body: what the
handler writes, streamed or not, is not sent. Responses with status 1xx, 204 or 304 have no body
either (and 204 no `Content-Length` or default `Content-Type`).

A proxy streams a body through without collecting it:

```ts
import { serveHttp, HttpRequest, HttpResponse } from "http-server";

async function main() {
  const server = await serveHttp({ port: 0 }, async (req: HttpRequest, res: HttpResponse) => {
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

## HTTP/2

`serveHttp` speaks HTTP/2 (RFC 9113) as well, with the same handlers: over TLS when the client
chooses it with ALPN (the listener offers `h2` and `http/1.1`, unless `tls.alpn` names the
protocols), and over plain TCP when the connection starts with the HTTP/2 preface (h2c with
prior knowledge: `curl --http2-prior-knowledge`). A request with `Upgrade: h2c` is answered over
HTTP/1.1: RFC 9113 deprecates that upgrade, and browsers use HTTP/2 only over TLS.
`http2: false` turns it off; `maxConcurrentStreams` (128) is the number of requests of one
connection whose handlers run at once.

```ts
import { serveHttp, HttpRequest, HttpResponse } from "http-server";

async function main() {
  const server = await serveHttp({ port: 0, maxConcurrentStreams: 256 }, async (
    req: HttpRequest,
    res: HttpResponse,
  ) => {
    res.send(`hello over ${req.header("host")}`);
  });
  server.close();
}
```

The HTTP/2 server is Velt code too (`std/net/http/server/h2/`): one task per connection, each
request's handler a promise on that task, frames parsed in place, HPACK with the dynamic table
and Huffman coding both ways, and the frames written while one read is processed sent in one
write. What differs from HTTP/1.1, for a handler:

- Requests of one connection run at once (up to `maxConcurrentStreams`); a later one does not
  wait for an earlier response.
- Header names arrive in lowercase; `header(name)` matches any case, and `host` is the request's
  `:authority` when no `host` header came. `headers()` leaves out the pseudo-headers.
- `keepAlive` is always true. The connection is never handed over: `isUpgrade(protocol)` is true
  for an extended CONNECT (RFC 8441: `:method CONNECT`, `:protocol websocket`; the server
  announces SETTINGS_ENABLE_CONNECT_PROTOCOL), and `upgrade(protocol)` answers it 200 and
  returns a `TcpStream` that carries the protocol over that one stream (one end of a
  `streamPair`, velt/net), within the stream's flow control both ways. `acceptWebSocket` of
  [velt/net/websocket](websocket.md) does this by itself, so a WebSocket server works over
  HTTP/1.1 and HTTP/2 alike.
- Bodies have flow control: a request body arrives as fast as the handler reads it (up to
  256 KiB a stream may send ahead), and a response body is sent as fast as the client's
  windows allow; `HttpStream.write` waits while more than 64 KiB are still to be sent.
- `requestTimeout` applies to each stream: a handler reading a body that has not arrived within
  it from the request's head gets `IoError` `ETIMEDOUT`, and the client 408 (if nothing was sent
  yet); the stream is then reset (NO_ERROR), the connection and its other streams go on.
  `idleTimeout` closes a connection without streams (GOAWAY first).
- A client that resets its stream aborts `res.signal()`, and a write to it throws `IoError`;
  the handler's slot stays taken until it returns, so resetting streams never runs more
  handlers at once than the limit.
- Header names a response may not have over HTTP/2 (`Connection`, `Keep-Alive`,
  `Transfer-Encoding`, `Upgrade`) are not sent.
- Request trailers are read and dropped; response trailers (`addTrailers`) are always sent.
  Server push is not offered.

Limits: a request head larger than `maxHeadSize` gets 431, a malformed request (an uppercase
header name, a connection-specific header, a missing `:path`, a `content-length` that does not
match the body) is reset without reaching the handler, and a connection that resets or opens
streams it may not, sends empty DATA frames, or floods CONTINUATION frames is closed with
`ENHANCE_YOUR_CALM` (rapid reset, CVE-2023-44487, and its relatives). `close()` sends GOAWAY:
streams in flight finish, then each connection closes. The design and its benchmarks are in
[docs/internals/design/http2.md](../internals/design/http2.md).

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
