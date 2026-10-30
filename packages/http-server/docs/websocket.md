# `http-server/websocket`

`import { isWebSocketRequest, upgradeWebSocket, acceptWebSocket } from "http-server/websocket"`.
Accepting WebSockets (RFC 6455) on the server: each is a
[velt/net/websocket](../../../docs/std/websocket.md) `WebSocket` (sending, receiving, topics) once
its handshake is answered, over an HTTP/1.1 `Upgrade`
([`upgradeConnection`](../README.md#upgradeconnectionreq-protocol-headers)) or, over HTTP/2, the
extended CONNECT of RFC 8441 (the socket then runs on that one stream).

- `isWebSocketRequest(req: Request): bool`.
- `upgradeWebSocket(req: Request): WebSocketUpgrade { socket: WebSocket; response: Response }`:
  return `response` (a `101`) from the handler and use `socket` from a spawned task. Throws
  `IoError` "EINVAL" if `req` is not an upgrade request.
- `acceptWebSocket(req: HttpRequest, res: HttpResponse): WebSocket | null`: the same on a
  [`serveHttp`](serve-http.md) server: answers the `101` itself and returns the socket
  (with the frames the client sent along with the request). A request that is not a valid
  handshake (RFC 6455 §4.2.1: `GET`, HTTP/1.1, `Upgrade: websocket`, `Connection: upgrade`,
  `Sec-WebSocket-Version: 13`, a `Sec-WebSocket-Key` of 16 base64-encoded bytes) is answered
  there, as RFC 6455 §4.2.2 allows: 426 with `Sec-WebSocket-Version: 13` when it is not an
  upgrade or asks for another version, else 400 (the `ws` package answers 400 to another
  version and 405 to another method); it then returns null. Throws `IoError`
  "EINVAL" if a response was started.

On the server side the socket works once the handler has returned the `101`: a `send` before
that is queued and goes out right after the response, in order, and a `receive` waits for it.

A chat room: every message a client sends goes to everyone else in the room.

```ts
import { serve } from "http-server";
import { WebSocket } from "velt/net/websocket";
import { isWebSocketRequest, upgradeWebSocket } from "http-server/websocket";

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

