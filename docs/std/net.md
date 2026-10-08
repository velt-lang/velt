# velt:net

`import { listen, connect } from "velt:net"`. TCP. `TcpListener` and `TcpStream` are
structs around a handle, like file descriptors. Release each handle with `close()`.

- `listen(addr: "host:port"): Promise<TcpListener>`: port 0 picks a free port.
  `connect(addr): Promise<TcpStream>`.
- `TcpListener { port }`: `accept(): Promise<TcpStream>` (TCP_NODELAY on), `close()`.
- `TcpStream`:
  - `read(max = 0): Promise<u8[]>`: empty means end of stream.
  - `readString(max = 0): Promise<string>`: UTF-8; a character split across reads is completed
    by the next read.
  - `write(data: string)`, `writeBytes(data: u8[])`.
  - `shutdown()`: half-close.
  - `peerAddr(): string`, `close()`.

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

Notes: an unclosed handle leaks until the process exits. After `close()` through any copy
(also one handed to another task), every copy's operations throw `IoError` `EBADF` and closing
again does nothing; operations already in flight finish first. **Planned**
([semantics stage 2 §7](../internals/design/semantics-stage2.md#7-identity-and-the-struct-keyword)):
these structs become disposable classes with `using` support.
