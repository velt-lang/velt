// TechEmpower Framework Benchmarks server on node:http + pg (Pool): /json, /plaintext, /db,
// /queries?queries=N, /fortunes and /updates?queries=N (bench/web/README.md has the rules).
// Usage: node server.mjs [port]; env PORT, HOST (127.0.0.1), DATABASE_URL, DB_POOL (default
// 2 × cores). CLUSTER=1 forks one worker per core (node:cluster, shared listening socket);
// DB_POOL is then split between the workers (at least 2 connections each).
import http from "node:http";
import cluster from "node:cluster";
import os from "node:os";
import pg from "pg";

const port = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const host = process.env.HOST ?? "127.0.0.1";
const cores = os.availableParallelism();
const poolTotal = Number(process.env.DB_POOL || cores * 2);
const clustered = process.env.CLUSTER === "1";
const url =
  process.env.DATABASE_URL ??
  "postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world";

const SELECT_WORLD = {
  name: "select-world",
  text: 'SELECT id, randomnumber AS "randomNumber" FROM world WHERE id = $1',
};
const SELECT_FORTUNES = { name: "select-fortunes", text: "SELECT id, message FROM fortune" };
// One statement for any N: the sorted ids and new numbers travel as two int arrays.
const UPDATE_WORLDS = {
  name: "update-worlds",
  text:
    "UPDATE world SET randomnumber = u.r FROM (SELECT unnest($1::int[]) AS id, " +
    "unnest($2::int[]) AS r) AS u WHERE world.id = u.id",
};
const FORTUNES_HEAD =
  "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";

const randomId = () => Math.floor(Math.random() * 10000) + 1;

function queryCount(u) {
  const q = u.indexOf("?");
  const n = q < 0 ? NaN : parseInt(new URLSearchParams(u.slice(q + 1)).get("queries") ?? "", 10);
  if (Number.isNaN(n) || n < 1) return 1;
  return n > 500 ? 500 : n;
}

const escapes = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const escapeHtml = (s) => s.replace(/[&<>"']/g, (c) => escapes[c]);

function makeHandlers(pool) {
  const fetchWorld = async () =>
    (await pool.query({ ...SELECT_WORLD, values: [randomId()] })).rows[0];
  const fetchWorlds = (n) => Promise.all(Array.from({ length: n }, fetchWorld));

  async function updateWorlds(n) {
    const worlds = await fetchWorlds(n);
    for (const w of worlds) w.randomNumber = randomId();
    const sorted = [...worlds].sort((a, b) => a.id - b.id);
    await pool.query({
      ...UPDATE_WORLDS,
      values: [sorted.map((w) => w.id), sorted.map((w) => w.randomNumber)],
    });
    return worlds;
  }

  async function fortunesHtml() {
    const fortunes = (await pool.query(SELECT_FORTUNES)).rows;
    fortunes.push({ id: 0, message: "Additional fortune added at request time." });
    fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
    let html = FORTUNES_HEAD;
    for (const f of fortunes) html += `<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`;
    return html + "</table></body></html>";
  }

  return { fetchWorlds, updateWorlds, fortunesHtml };
}

function send(res, type, body, status = 200) {
  res.writeHead(status, {
    "content-type": type,
    "content-length": Buffer.byteLength(body),
    server: "node",
  });
  res.end(body);
}

async function route(db, req, res) {
  const path = req.url.split("?", 1)[0];
  switch (path) {
    case "/plaintext":
      return send(res, "text/plain; charset=utf-8", "Hello, World!");
    case "/json":
      return send(res, "application/json", JSON.stringify({ message: "Hello, World!" }));
    case "/db":
      return send(res, "application/json", JSON.stringify((await db.fetchWorlds(1))[0]));
    case "/queries":
      return send(res, "application/json", JSON.stringify(await db.fetchWorlds(queryCount(req.url))));
    case "/updates":
      return send(res, "application/json", JSON.stringify(await db.updateWorlds(queryCount(req.url))));
    case "/fortunes":
      return send(res, "text/html; charset=utf-8", await db.fortunesHtml());
    default:
      return send(res, "text/plain; charset=utf-8", "not found", 404);
  }
}

function startServer(poolSize) {
  const pool = new pg.Pool({ connectionString: url, max: poolSize });
  const db = makeHandlers(pool);
  http
    .createServer((req, res) => {
      route(db, req, res).catch((e) => {
        console.error("request failed:", e.message);
        if (!res.headersSent) send(res, "text/plain; charset=utf-8", "internal error", 500);
      });
    })
    .listen(port, host, () => console.log(`listening on http://${host}:${port}`));
}

if (clustered && cluster.isPrimary) {
  for (let i = 0; i < cores; i++) cluster.fork();
} else {
  startServer(clustered ? Math.max(2, Math.ceil(poolTotal / cores)) : poolTotal);
}
