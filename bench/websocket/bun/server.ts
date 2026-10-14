// The Bun server (bench/websocket/README.md): Bun.serve's built-in WebSockets (uWebSockets).
//   /echo   sends every message back to its sender
//   /bcast  sends every message to every connection on /bcast (a pub/sub topic)
// Usage: bun server.ts [port]; env PORT, HOST (127.0.0.1).
const port = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const hostname = process.env.HOST ?? "127.0.0.1";

type Data = { bcast: boolean };

const server = Bun.serve<Data>({
  port,
  hostname,
  fetch(req, server) {
    const bcast = new URL(req.url).pathname === "/bcast";
    if (server.upgrade(req, { data: { bcast } })) return;
    return new Response("websocket bench server: /echo or /bcast\n");
  },
  websocket: {
    perMessageDeflate: false,
    open(ws) {
      if (ws.data.bcast) ws.subscribe("room");
    },
    message(ws, msg) {
      // server.publish reaches the sender too, like the other servers' rooms.
      if (ws.data.bcast) server.publish("room", msg);
      else ws.send(msg);
    },
  },
});
console.log(`listening on ws://${hostname}:${server.port}`);
