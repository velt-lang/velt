// The Node server (bench/websocket/README.md): node:http + ws, the usual Node WebSocket library.
//   /echo   sends every message back to its sender
//   /bcast  sends every message to every connection on /bcast (a chat room)
// Usage: node server.mjs [port]; env PORT, HOST (127.0.0.1).
import http from "node:http";
import { WebSocketServer } from "ws";

const port = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const host = process.env.HOST ?? "127.0.0.1";

const wss = new WebSocketServer({ noServer: true, perMessageDeflate: false });
const room = new Set();

wss.on("connection", (ws, req) => {
  ws.on("error", () => {});
  if (req.url === "/bcast") {
    room.add(ws);
    ws.on("message", (data, isBinary) => {
      for (const s of room) s.send(data, { binary: isBinary });
    });
    ws.on("close", () => room.delete(ws));
  } else {
    ws.on("message", (data, isBinary) => ws.send(data, { binary: isBinary }));
  }
});

const server = http.createServer((req, res) => res.end("websocket bench server: /echo or /bcast\n"));
server.on("upgrade", (req, socket, head) => {
  wss.handleUpgrade(req, socket, head, (ws) => wss.emit("connection", ws, req));
});
server.listen(port, host, () => console.log(`listening on ws://${host}:${port}`));
