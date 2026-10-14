// chatbench server, Bun version: Bun.serve + built-in WebSocket pub/sub + bun:sqlite.
// Contract: ../SPEC.md.
import { Database } from "bun:sqlite";
import { createHmac } from "node:crypto";
import { rmSync } from "node:fs";
import type { ServerWebSocket } from "bun";

const PORT = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const HOST = process.env.HOST ?? "127.0.0.1";
const DB_PATH = process.env.DB ?? "chat.db";
const SECRET = "chatbench-secret";
const NAME_RE = /^[a-z0-9]{1,32}$/;
const HISTORY = 20;
const BAD_MESSAGE = JSON.stringify({ type: "error", error: "bad message" });

// --- SQLite -----------------------------------------------------------------
for (const suffix of ["", "-wal", "-shm"]) {
  rmSync(DB_PATH + suffix, { force: true });
}
const db = new Database(DB_PATH, { create: true });
db.run("PRAGMA journal_mode = WAL");
db.run("PRAGMA synchronous = NORMAL");
db.run(
  "CREATE TABLE messages (id INTEGER PRIMARY KEY, room TEXT NOT NULL, user TEXT NOT NULL, " +
    "text TEXT NOT NULL, ts INTEGER NOT NULL)",
);
db.run("CREATE INDEX messages_room_id ON messages(room, id)");
const insertStmt = db.prepare("INSERT INTO messages (room, user, text, ts) VALUES (?, ?, ?, ?)");
const historyStmt = db.prepare(
  "SELECT id, user, text, ts FROM messages WHERE room = ? ORDER BY id DESC LIMIT " + HISTORY,
);

// --- shared state -----------------------------------------------------------
// Bun's pub/sub owns room membership; this map only counts members per room for /stats.
const roomSizes = new Map<string, number>();
let connections = 0;
let messagesStored = 0;

type Conn = { user: string; room: string };

// --- helpers ----------------------------------------------------------------
function checkToken(token: string | null): string | null {
  if (token === null) return null;
  const dot = token.indexOf(".");
  if (dot < 0) return null;
  const user = token.slice(0, dot);
  const sig = token.slice(dot + 1);
  const expected = createHmac("sha256", SECRET).update(user).digest("hex");
  return sig === expected ? user : null;
}

/** 1-1000 characters (Unicode code points). */
function validText(s: unknown): s is string {
  if (typeof s !== "string" || s.length === 0) return false;
  if (s.length <= 1000) return true; // code points <= UTF-16 units
  if (s.length > 2000) return false;
  let n = 0;
  for (const _ of s) if (++n > 1000) return false;
  return true;
}

// --- server -----------------------------------------------------------------
// HTTPS/WSS when TLS_CERT and TLS_KEY name PEM files, else plain HTTP/WS.
const TLS =
  process.env.TLS_CERT && process.env.TLS_KEY
    ? { cert: Bun.file(process.env.TLS_CERT), key: Bun.file(process.env.TLS_KEY) }
    : undefined;

const server = Bun.serve({
  port: PORT,
  hostname: HOST,
  tls: TLS,
  fetch(req, server) {
    const url = new URL(req.url);
    if (url.pathname === "/stats" && req.method === "GET") {
      return Response.json({ connections, rooms: roomSizes.size, messages: messagesStored });
    }
    if (url.pathname !== "/ws") return new Response(null, { status: 404 });
    const user = checkToken(url.searchParams.get("token"));
    if (user === null) return new Response(null, { status: 401 });
    const room = url.searchParams.get("room");
    if (!NAME_RE.test(user) || room === null || !NAME_RE.test(room)) {
      return new Response(null, { status: 400 });
    }
    if (server.upgrade(req, { data: { user, room } })) return undefined;
    return new Response(null, { status: 426 });
  },
  websocket: {
    perMessageDeflate: false,
    open(ws: ServerWebSocket<Conn>) {
      const { user, room } = ws.data;
      connections++;
      roomSizes.set(room, (roomSizes.get(room) ?? 0) + 1);
      ws.subscribe(room);
      const history = (historyStmt.all(room) as object[]).reverse();
      ws.send(JSON.stringify({ type: "welcome", user, room, history }));
    },
    message(ws: ServerWebSocket<Conn>, data: string | Buffer) {
      if (typeof data !== "string") return void ws.send(BAD_MESSAGE);
      let msg: any;
      try {
        msg = JSON.parse(data);
      } catch {
        return void ws.send(BAD_MESSAGE);
      }
      if (msg === null || typeof msg !== "object") return void ws.send(BAD_MESSAGE);
      if (msg.type === "say") {
        const text = msg.text;
        if (!validText(text)) return void ws.send(BAD_MESSAGE);
        const { user, room } = ws.data;
        const ts = Date.now();
        const id = Number(insertStmt.run(room, user, text, ts).lastInsertRowid);
        messagesStored++;
        server.publish(room, JSON.stringify({ type: "message", id, room, user, text, ts }));
      } else if (msg.type === "ping" && typeof msg.t === "number") {
        ws.send(JSON.stringify({ type: "pong", t: msg.t }));
      } else {
        ws.send(BAD_MESSAGE);
      }
    },
    close(ws: ServerWebSocket<Conn>) {
      const { room } = ws.data;
      connections--;
      const n = (roomSizes.get(room) ?? 1) - 1;
      if (n <= 0) roomSizes.delete(room);
      else roomSizes.set(room, n);
      // Bun drops the closed socket's subscriptions itself.
    },
  },
});

console.log(`chatbench bun listening on ${TLS ? "https" : "http"}://${HOST}:${server.port} (db ${DB_PATH})`);
