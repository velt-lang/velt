# velt:udp

`import { bindUdp } from "velt:udp"`. UDP sockets. Addresses are `"host:port"` strings.
`UdpSocket` is a handle like `TcpStream`: it can be used from several tasks and is released
by `close()`.

- `bindUdp(addr): Promise<UdpSocket>`: port 0 picks a free port.
- `UdpSocket { port: number }`:
  - `sendTo(data: u8[], addr): Promise<i64>`, `sendTextTo(text, addr)`
  - `recvFrom(max = 0): Promise<Datagram>`
  - `setBroadcast(on)`, `close()`
- `Datagram { data: u8[]; addr }`: `text()` decodes the payload as UTF-8.

```ts
import { bindUdp } from "velt:udp";

async function main() {
  const server = await bindUdp("127.0.0.1:0");
  const client = await bindUdp("127.0.0.1:0");
  await client.sendTextTo("ping", `127.0.0.1:${server.port}`);
  const d = await server.recvFrom();
  await server.sendTextTo("pong", d.addr);
  console.log(d.text(), (await client.recvFrom()).text()); // ping pong
  client.close();
  server.close();
}
```
