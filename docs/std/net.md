# velt/net

`import { listen, connect } from "velt/net"`. TCP, and TLS over TCP. `TcpListener` and
`TcpStream` are structs around a handle, like file descriptors. Release each handle with
`close()`.

- `listen(addr: "host:port"): Promise<TcpListener>`: port 0 picks a free port. The listen
  backlog is the OS maximum (a burst of connects waits to be accepted).
  `connect(addr): Promise<TcpStream>`.
- `TcpListener { port: number }`: `accept(): Promise<TcpStream>` (TCP_NODELAY on), `close()`
  (an `accept` still waiting then fails with `EBADF`).
- `TcpStream`:
  - `read(max = 0): Promise<u8[]>`: empty means end of stream.
  - `readString(max = 0): Promise<string>`: UTF-8; a character split across reads is completed
    by the next read.
  - `readInto(buf: Uint8Array): Promise<number | null>`: reads what has arrived into `buf` (at
    most `buf.length` bytes) and returns how many, or `null` at the end of the stream, as
    Deno's `conn.read`. It waits only when nothing has arrived yet, and a plain TCP stream reads
    straight into `buf`: nothing is allocated or copied on the way. Reuse one buffer per
    connection and parse with `subarray` views of what was read. An empty `buf` returns 0.
    `read` and `readInto` may be mixed on one stream.
  - `tryRead(buf: u8[] | Uint8Array, start = 0, end = buf.length): number | null`: reads what
    has arrived into `buf[start..end]` without waiting: the bytes read, `0` when nothing has
    arrived yet, `null` at the end of the stream (`RangeError` for an empty or out-of-range
    range). With `readable(): Promise<void>`, which waits until a `tryRead` may find something,
    a parser reads straight into its own `u8[]` buffer.
  - `write(data: string)`, `writeBytes(data: u8[] | Uint8Array, start = 0, end = data.length)`: bytes go out in the order of
    the calls, also when a write is started and not awaited yet. What the socket takes at the
    call is written from `data` itself; only the rest is copied. A `Uint8Array` writes its own
    range (a `subarray` writes just that part); `start` and `end` write part of either (taken
    as `subarray` takes them: a negative one counts from the end, and both are clamped to it), so a reply buffer kept per connection goes out without a copy.
  - `tryWrite(data: u8[] | Uint8Array, start = 0, end = data.length): i64`: writes what the socket takes right now, without waiting, and
    returns the number of bytes written (0 when it takes nothing now, while an earlier write is
    still going on, for a TLS stream, and after an error, which the next `write` reports). When
    everything was written there is nothing to await.
  - `writeLater(data: string): i64`, `writeBytesLater(data: u8[] | Uint8Array, start = 0, end =
    data.length): i64`: queue bytes (copied at the call) that are sent when the task next waits
    or finishes, together with everything else it queued until then: one system call for a
    response head and the body pieces a render produced without waiting, and nothing held back
    past a wait. They return the number of bytes still to be sent on the stream. A `write` goes
    after what was queued before it. They throw `IoError` once sending failed (the error a
    `write` would report), after `abort()` and on a closed handle.
  - `drained(max = 0): Promise<void>`: resolves once at most `max` bytes queued by `writeLater`
    (or written after them) are still to be sent, at once when there are no more: keeps a
    producer that queues faster than the peer reads bounded. Throws `IoError` when sending
    failed.
  - `shutdown()`: half-close, after every byte written or queued before it (a TLS stream then
    sends its close_notify).
  - `abort()`: fails the operations in flight (a `write` to a peer that stopped reading would
    otherwise never finish) and every later one. The handle stays open until `close()`.
  - `readableWithin(ms, signal?: AbortSignal): Promise<bool>`: `readable()` with a limit: true
    once a `tryRead` may find something, false when `ms` milliseconds passed first or `signal` was
    aborted. One wait in the runtime, so nothing is left behind (a `Promise.race` of `readable()`
    and a timer leaves the losing wait running): a server waiting for each idle connection's next
    request.
  - `peerAddr(): string`, `close()`: releases the handle; a read or `readable()` still waiting
    then fails with `EBADF` (no code can read what it would bring), a write in flight goes on,
    and so do bytes queued with `writeLater` (`await drained()` first to know they were sent).
- `connectTls(addr, opts?: TlsConnectOptions { serverName?; ca?; alpn?: string[] }):
  Promise<TcpStream>`: a TLS connection (rustls), resolved once the handshake is done. The
  certificate is checked for the host in `addr` (or `serverName`) against the built-in roots
  plus the PEM certificates in `ca`.
- `listenTls(addr, opts: TlsListenOptions { cert; key; alpn?: string[] }):
  Promise<TcpListener>`: a listener whose `accept` gives TLS streams (`cert` and `key` are PEM
  text; bad PEM throws `IoError` `EINVAL`). `accept` returns as soon as the TCP connection is
  there: the handshake runs with the stream's first read or write, which throws if it fails
  or takes more than 10 s.

A TLS stream is a `TcpStream` like any other: one task can read while another writes, and
`shutdown()` sends TLS's close_notify. `alpn` lists the protocols to offer, most preferred
first, and `await stream.alpnProtocol()` is the one the handshake chose (`"h2"`,
`"http/1.1"`), or null when none was agreed or the stream is not TLS (Node's
`socket.alpnProtocol`): it waits for the handshake, running it if no read or write did yet, and
throws if it fails. A server that offers HTTP/2 and HTTP/1.1 reads it to know which one the
client speaks:

```ts
import { listenTls, TcpListener } from "velt/net";

async function protocolOf(l: TcpListener): Promise<string> {
  const conn = await l.accept();
  const p = await conn.alpnProtocol();
  conn.close();
  return p ?? "http/1.1";
}
```

```ts
import { listen, connect, TcpListener } from "velt/net";

async function echoOnce(l: TcpListener): Promise<void> {
  const conn = await l.accept();
  const msg = await conn.readString();
  await conn.write(msg.toUpperCase());
  conn.close();
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const server = spawn(echoOnce(l));
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write("ping");
  console.log(await c.readString()); // PING
  c.close();
  await server;
  l.close();
}
```

```ts
import { listenTls, connectTls } from "velt/net";
import { readFile } from "velt/fs";

async function main() {
  const l = await listenTls("127.0.0.1:0", {
    cert: await readFile("cert.pem"),
    key: await readFile("key.pem"),
  });
  const server = spawn(async () => {
    const conn = await l.accept();
    await conn.write(`hello, ${await conn.readString()}`);
    conn.close();
  });
  const c = await connectTls(`localhost:${l.port}`, { ca: await readFile("ca.pem") });
  await c.write("world");
  console.log(await c.readString()); // hello, world
  c.close();
  await server;
  l.close();
}
```

A connection taken over from an HTTP server
([`upgradeConnection`](http.md#upgradeconnectionreq-protocol-headers)) is a `TcpStream` too.

Notes: an unclosed handle leaks until the process exits. After `close()` through any copy
(also one handed to another task), every copy's operations throw `IoError` `EBADF` and closing
again does nothing; operations already in flight finish first. **Planned**
([semantics stage 2 §7](../internals/design/semantics-stage2.md#7-identity-and-the-struct-keyword)):
these structs become disposable classes with `using` support.
