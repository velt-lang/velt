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
  and releases the handle; always call it).
- `WsMessage { isBinary; text; data: u8[] }`.

```ts
import { serve, Request, Response } from "velt:http";
import { WebSocket, isWebSocketRequest, upgradeWebSocket, connectWebSocket } from "velt:websocket";

async function echo(ws: WebSocket): Promise<void> {
  let m = await ws.receive();
  while (m != null) {
    await ws.send(m.text);
    m = await ws.receive();
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
    return Response.text("websocket only", 400);
  });
  const ws = await connectWebSocket(`ws://127.0.0.1:${server.port}/`);
  await ws.send("hi");
  console.log((await ws.receive())?.text ?? ""); // hi
  await ws.close();
  server.close();
}
```

Notes: messages are pulled with `receive` (no callbacks, so hot reload needs nothing special);
pings are answered automatically; WebSockets over HTTP/2 (RFC 8441) are not supported.
