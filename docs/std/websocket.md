# velt:websocket

`import { isWebSocketRequest, upgradeWebSocket, connectWebSocket, WebSocket } from
"velt:websocket"`. WebSockets (RFC 6455): accepted by a velt:http server through an HTTP/1.1
upgrade, or opened as a client to `ws://` / `wss://` URLs.

- `isWebSocketRequest(req: Request): bool`.
- `upgradeWebSocket(req: Request): WebSocketUpgrade { socket: WebSocket; response: Response }`:
  return `response` (a `101`) from the handler and use `socket` from a spawned task. Throws
  `IoError` "EINVAL" if `req` is not an upgrade request.
- `connectWebSocket(url, opts: WsConnectOptions { ca? } = {}): Promise<WebSocket>`.
- `WebSocket` (a handle): `send(text)`, `sendBytes(data: u8[])`, `receive(): Promise<WsMessage
  | null>` (null once the peer closed), `close(code = 1000, reason = "")` (sends a close frame
  and releases the handle; always call it). `code` must be one a peer may receive (RFC 6455
  §7.4): 1000–1003, 1007–1014 or 3000–4999; others throw `IoError` "EINVAL", send nothing and leave the connection open.
  `reason` is cut to 123 bytes at a character boundary. A `WebSocket` is an `AsyncIterable<WsMessage,
  IoError>`: `for await (const m of ws)` receives messages until the peer closes; leaving the
  loop early leaves the connection open.
- `WsMessage { isBinary; text; data: u8[] }`.
- Topics: `await ws.subscribe(topic)`, `ws.unsubscribe(topic)`; `publish(topic, text)` and
  `publishBytes(topic, data)` send to every subscriber, `ws.publish(topic, text)` /
  `ws.publishBytes` to every subscriber but `ws` (like Bun's `server.publish` and
  `ws.publish`: the same connections receive the message). They return the number of
  connections the message was queued for (Bun's return bytes sent, 0 or −1): the frame is
  encoded once and queued for each subscriber, so a publish costs no task or promise per
  socket. A subscriber with more than 16 MiB already queued is skipped for that message, not
  closed (a slow reader can't grow the server's memory). A connection stays subscribed until
  `unsubscribe` or until `close()` releases it, also after the peer disconnected while the
  handler still holds the handle.

```ts
import { serve } from "velt:http";
import { WebSocket, isWebSocketRequest, upgradeWebSocket, connectWebSocket } from "velt:websocket";

async function echo(ws: WebSocket): Promise<void> {
  for await (const m of ws) {
    await ws.send(m.text);
  }
  await ws.close();
}

async function main() {
  const server = await serve({ port: 0 }, async (req: Request): Promise<Response> => {
    if (isWebSocketRequest(req)) {
      const up = upgradeWebSocket(req);
      spawn(echo(up.socket));
      return up.response;
    }
    return new Response("websocket only", { status: 400 });
  });
  const ws = await connectWebSocket(`ws://127.0.0.1:${server.port}/`);
  await ws.send("hi");
  console.log((await ws.receive())?.text ?? ""); // hi
  await ws.close();
  server.close();
}
```

A chat room: every message a client sends goes to everyone else in the room.

```ts
import { serve } from "velt:http";
import { WebSocket, isWebSocketRequest, upgradeWebSocket } from "velt:websocket";

async function member(ws: WebSocket): Promise<void> {
  await ws.subscribe("room");
  for await (const m of ws) {
    ws.publish("room", m.text);
  }
  await ws.close();
}

async function main() {
  const server = await serve({ port: 0 }, async (req: Request): Promise<Response> => {
    if (!isWebSocketRequest(req)) {
      return new Response("chat room", { status: 400 });
    }
    const up = upgradeWebSocket(req);
    spawn(member(up.socket));
    return up.response;
  });
  console.log(`listening on ${server.port}`);
  await server.shutdown();
}
```

Notes: messages are pulled with `receive` or `for await` (no callbacks, so hot reload needs nothing special);
pings are answered automatically; WebSockets over HTTP/2 (RFC 8441) are not supported.

`send` and `sendBytes` queue the message when they are called, so messages go out in call order
(also `await Promise.all([ws.send(a), ws.send(b)])`; `spawn(ws.send(x))` sends when the spawned
task runs, while `const p = ws.send(x)` queues at the call), and return once it is queued, as `ws.send`
does in browsers, Node and Bun: everything queued on a connection goes out in one write, so replies to messages that
arrived together, or a message sent to many sockets, cost one system call per socket. A send
that leaves more than 1 MiB queued for the connection completes only once the writer has caught
up, so a slow peer slows its sender down instead of filling memory. A connection that broke is reported by the next `send` or `receive`
(`IoError`). `close` returns once everything sent before it is written; a peer that stops reading gets 30
seconds to take what is queued after `close` (or after the handle is released), then the
connection is shut down. An idle connection keeps
no read buffer (about 3 KB of memory per open connection in total).

To send one message to many connections, `publish` to a topic they subscribed to. To send to a
list you keep yourself, a plain loop is the fastest way
(each `await` returns at once; `Promise.all` over the sends adds a promise per socket):

```ts
import { WebSocket } from "velt:websocket";

async function broadcast(sockets: WebSocket[], text: string) {
  for (const s of sockets) {
    try {
      await s.send(text);
    } catch (e) {
      // That peer is gone; its own receive loop cleans it up.
    }
  }
}
```
