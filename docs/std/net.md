# velt:net

`import { listen, connect } from "velt:net"`. TCP, and TLS over TCP. `TcpListener` and
`TcpStream` are structs around a handle, like file descriptors. Release each handle with
`close()`.

- `listen(addr: "host:port"): Promise<TcpListener>`: port 0 picks a free port. The listen
  backlog is the OS maximum (a burst of connects waits to be accepted).
  `connect(addr): Promise<TcpStream>`.
- `TcpListener { port: number }`: `accept(): Promise<TcpStream>` (TCP_NODELAY on), `close()`.
- `TcpStream`:
  - `read(max = 0): Promise<u8[]>`: empty means end of stream.
  - `readString(max = 0): Promise<string>`: UTF-8; a character split across reads is completed
    by the next read.
  - `write(data: string)`, `writeBytes(data: u8[])`.
  - `tryWrite(data: u8[]): i64`: writes what the socket takes right now, without waiting, and
    returns the number of bytes written (0 when it takes nothing now, for a TLS stream, and
    after an error, which the next `write` reports). When everything was written there is
    nothing to await.
  - `shutdown()`: half-close.
  - `abort()`: fails the operations in flight (a `write` to a peer that stopped reading would
    otherwise never finish; a `read` may see the end of the stream instead) and every later
    one. The handle stays open until `close()`.
  - `peerAddr(): string`, `close()`.
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
first.

```ts
import { listen, connect, TcpListener } from "velt:net";

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
import { listenTls, connectTls } from "velt:net";
import { readFile } from "velt:fs";

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
