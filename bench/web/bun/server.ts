// TechEmpower Framework Benchmarks server on Bun.serve + Bun's built-in Postgres client
// (Bun.SQL): /json, /plaintext, /db, /queries?queries=N, /fortunes and /updates?queries=N
// (bench/web/README.md has the rules).
// Usage: bun server.ts [port]; env PORT, HOST (127.0.0.1), DATABASE_URL, DB_POOL (default
// 2 × cores). REUSE_PORT=1 sets reusePort, so several processes can share the port (Linux
// balances between them; macOS does not).
import { SQL } from "bun";
import os from "node:os";

const port = Number(process.argv[2] ?? process.env.PORT ?? 8080);
const hostname = process.env.HOST ?? "127.0.0.1";
const poolSize = Number(process.env.DB_POOL || os.availableParallelism() * 2);
const sql = new SQL({
  url:
    process.env.DATABASE_URL ??
    "postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world",
  max: poolSize,
  prepare: true,
});

const FORTUNES_HEAD =
  "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";

type World = { id: number; randomNumber: number };
type Fortune = { id: number; message: string };

const randomId = () => Math.floor(Math.random() * 10000) + 1;

function queryCount(url: URL): number {
  const n = parseInt(url.searchParams.get("queries") ?? "", 10);
  if (Number.isNaN(n) || n < 1) return 1;
  return n > 500 ? 500 : n;
}

async function fetchWorld(): Promise<World> {
  const rows =
    await sql`SELECT id, randomnumber AS "randomNumber" FROM world WHERE id = ${randomId()}`;
  return rows[0];
}

const fetchWorlds = (n: number) => Promise.all(Array.from({ length: n }, fetchWorld));

// Postgres array literal `{a,b,c}` (bound as text, cast by the server).
const intArray = (xs: number[]) => `{${xs.join(",")}}`;

async function updateWorlds(n: number): Promise<World[]> {
  const worlds = await fetchWorlds(n);
  for (const w of worlds) w.randomNumber = randomId();
  const sorted = [...worlds].sort((a, b) => a.id - b.id);
  const ids = intArray(sorted.map((w) => w.id));
  const values = intArray(sorted.map((w) => w.randomNumber));
  await sql`UPDATE world SET randomnumber = u.r FROM (SELECT unnest(${ids}::int[]) AS id, unnest(${values}::int[]) AS r) AS u WHERE world.id = u.id`;
  return worlds;
}

const escapes: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};
const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => escapes[c]);

async function fortunesHtml(): Promise<string> {
  const fortunes: Fortune[] = [...(await sql`SELECT id, message FROM fortune`)];
  fortunes.push({ id: 0, message: "Additional fortune added at request time." });
  fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
  let html = FORTUNES_HEAD;
  for (const f of fortunes) html += `<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`;
  return html + "</table></body></html>";
}

const TEXT = { "content-type": "text/plain; charset=utf-8", server: "bun" };
const JSON_TYPE = { "content-type": "application/json", server: "bun" };
const HTML = { "content-type": "text/html; charset=utf-8", server: "bun" };
const json = (v: unknown) => new Response(JSON.stringify(v), { headers: JSON_TYPE });

async function route(req: Request): Promise<Response> {
  const url = new URL(req.url);
  switch (url.pathname) {
    case "/plaintext":
      return new Response("Hello, World!", { headers: TEXT });
    case "/json":
      return json({ message: "Hello, World!" });
    case "/db":
      return json((await fetchWorlds(1))[0]);
    case "/queries":
      return json(await fetchWorlds(queryCount(url)));
    case "/updates":
      return json(await updateWorlds(queryCount(url)));
    case "/fortunes":
      return new Response(await fortunesHtml(), { headers: HTML });
    default:
      return new Response("not found", { status: 404, headers: TEXT });
  }
}

Bun.serve({
  port,
  hostname,
  reusePort: process.env.REUSE_PORT === "1",
  fetch: route,
  error(e) {
    console.error("request failed:", e.message);
    return new Response("internal error", { status: 500, headers: TEXT });
  },
});
console.log(`listening on http://${hostname}:${port}`);
