// chatbench server, Node version: node:http + ws + better-sqlite3.
// Contract: ../SPEC.md.
import http from "node:http";
import https from "node:https";
import fs from "node:fs";
import { createHmac } from "node:crypto";
import { WebSocketServer } from "ws";
import Database from "better-sqlite3";

const PORT = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const HOST = process.env.HOST ?? "127.0.0.1";
const DB_PATH = process.env.DB ?? "chat.db";
const SECRET = "chatbench-secret";
const NAME_RE = /^[a-z0-9]{1,32}$/;
const HISTORY = 20;
const BAD_MESSAGE = JSON.stringify({ type: "error", error: "bad message" });

// --- SQLite -----------------------------------------------------------------
for (const suffix of ["", "-wal", "-shm"]) {
  fs.rmSync(DB_PATH + suffix, { force: true });
}
const db = new Database(DB_PATH);
db.pragma("journal_mode = WAL");
db.pragma("synchronous = NORMAL");
db.exec(
  "CREATE TABLE messages (id INTEGER PRIMARY KEY, room TEXT NOT NULL, user TEXT NOT NULL, " +
    "text TEXT NOT NULL, ts INTEGER NOT NULL)",
);
db.exec("CREATE INDEX messages_room_id ON messages(room, id)");
const insertStmt = db.prepare("INSERT INTO messages (room, user, text, ts) VALUES (?, ?, ?, ?)");
const historyStmt = db.prepare(
  "SELECT id, user, text, ts FROM messages WHERE room = ? ORDER BY id DESC LIMIT " + HISTORY,
);

// --- shared state -----------------------------------------------------------
/** @type {Map<string, Set<import("ws").WebSocket>>} */
const rooms = new Map();
let connections = 0;
let messagesStored = 0;

// --- helpers ----------------------------------------------------------------
/** Returns the user for a valid token, or null. */
function checkToken(token) {
  if (typeof token !== "string") return null;
  const dot = token.indexOf(".");
  if (dot < 0) return null;
  const user = token.slice(0, dot);
  const sig = token.slice(dot + 1);
  const expected = createHmac("sha256", SECRET).update(user).digest("hex");
  return sig === expected ? user : null;
}

/** 1-1000 characters (Unicode code points). */
function validText(s) {
  if (typeof s !== "string" || s.length === 0) return false;
  if (s.length <= 1000) return true; // code points <= UTF-16 units
  if (s.length > 2000) return false;
  let n = 0;
  for (const _ of s) if (++n > 1000) return false;
  return true;
}

function reject(socket, status, reason) {
  socket.end(
    `HTTP/1.1 ${status} ${reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`,
  );
}

// --- HTTP -------------------------------------------------------------------
// HTTPS/WSS when TLS_CERT and TLS_KEY name PEM files, else plain HTTP/WS.
const TLS =
  process.env.TLS_CERT && process.env.TLS_KEY
    ? { cert: fs.readFileSync(process.env.TLS_CERT), key: fs.readFileSync(process.env.TLS_KEY) }
    : null;

const onRequest = (req, res) => {
  const path = req.url.split("?", 1)[0];
  if (req.method === "GET" && path === "/stats") {
    const body = JSON.stringify({ connections, rooms: rooms.size, messages: messagesStored });
    res.writeHead(200, { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) });
    res.end(body);
  } else if (path === "/ws") {
    res.writeHead(426, { "Content-Length": 0 });
    res.end();
  } else {
    res.writeHead(404, { "Content-Length": 0 });
    res.end();
  }
};
const server = TLS ? https.createServer(TLS, onRequest) : http.createServer(onRequest);

const wss = new WebSocketServer({ noServer: true, perMessageDeflate: false });

server.on("upgrade", (req, socket, head) => {
  socket.on("error", () => {});
  const q = req.url.indexOf("?");
  const path = q < 0 ? req.url : req.url.slice(0, q);
  if (path !== "/ws") return reject(socket, 404, "Not Found");
  const params = new URLSearchParams(q < 0 ? "" : req.url.slice(q + 1));
  const user = checkToken(params.get("token"));
  if (user === null) return reject(socket, 401, "Unauthorized");
  const room = params.get("room");
  if (!NAME_RE.test(user) || room === null || !NAME_RE.test(room)) {
    return reject(socket, 400, "Bad Request");
  }
  wss.handleUpgrade(req, socket, head, (ws) => onConnect(ws, user, room));
});

// --- WebSocket --------------------------------------------------------------
function onConnect(ws, user, room) {
  connections++;
  let members = rooms.get(room);
  if (members === undefined) {
    members = new Set();
    rooms.set(room, members);
  }
  members.add(ws);

  const history = historyStmt.all(room).reverse();
  ws.send(JSON.stringify({ type: "welcome", user, room, history }));

  let left = false;
  const leave = () => {
    if (left) return;
    left = true;
    connections--;
    members.delete(ws);
    if (members.size === 0 && rooms.get(room) === members) rooms.delete(room);
  };
  ws.on("close", leave);
  ws.on("error", leave);

  ws.on("message", (data, isBinary) => {
    if (isBinary) return ws.send(BAD_MESSAGE);
    let msg;
    try {
      msg = JSON.parse(data.toString("utf8"));
    } catch {
      return ws.send(BAD_MESSAGE);
    }
    if (msg === null || typeof msg !== "object") return ws.send(BAD_MESSAGE);
    if (msg.type === "say") {
      const text = msg.text;
      if (!validText(text)) return ws.send(BAD_MESSAGE);
      const ts = Date.now();
      const id = Number(insertStmt.run(room, user, text, ts).lastInsertRowid);
      messagesStored++;
      const out = JSON.stringify({ type: "message", id, room, user, text, ts });
      for (const peer of members) {
        if (peer.readyState === 1) peer.send(out);
      }
    } else if (msg.type === "ping" && typeof msg.t === "number") {
      ws.send(JSON.stringify({ type: "pong", t: msg.t }));
    } else {
      ws.send(BAD_MESSAGE);
    }
  });
}

server.listen(PORT, HOST, () => {
  console.log(`chatbench node listening on ${TLS ? "https" : "http"}://${HOST}:${PORT} (db ${DB_PATH})`);
});
