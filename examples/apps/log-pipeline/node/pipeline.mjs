// Node port of log-pipeline, written the way a TS/JS developer would (readline, regex literals,
// Map, sort with a comparator). Same input, same report bytes as the Velt version.
//   node node/pipeline.mjs gen access.log 1000000 [seed]
//   node node/pipeline.mjs report access.log
import { createReadStream, createWriteStream } from "node:fs";
import { createInterface } from "node:readline";
import { once } from "node:events";

class Random {
  constructor(seed) {
    this.state = seed >>> 0;
  }
  next(n) {
    this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0;
    return (this.state >>> 8) % n;
  }
}

const PATHS = [
  "/",
  "/api/users/{id}",
  "/api/users/{id}/orders",
  "/api/orders/{id}",
  "/api/products?page={n}",
  "/static/app.js",
  "/static/style.css",
  "/login",
  "/api/search?q=term{n}",
  "/health",
];

const pad2 = (n) => String(n).padStart(2, "0");

function pick(r, weights) {
  let x = r.next(100);
  for (let i = 0; i < weights.length; i++) {
    if (x < weights[i]) return i;
    x -= weights[i];
  }
  return weights.length - 1;
}

function client(r) {
  if (r.next(5) === 0) return `192.168.0.${r.next(50)}`;
  return `10.${r.next(4)}.${r.next(256)}.${r.next(256)}`;
}

function requestPath(r) {
  const template = PATHS[r.next(PATHS.length)];
  return template.replace("{id}", `${r.next(5000)}`).replace("{n}", `${r.next(20)}`);
}

function logLine(r, i, count) {
  if (r.next(1000) === 0) return `garbage line ${i}`;
  const second = Math.trunc((i * 86400) / count);
  const time = `30/Sep/2026:${pad2(Math.trunc(second / 3600))}:${pad2(Math.trunc(second / 60) % 60)}:${pad2(second % 60)}`;
  const method = ["GET", "POST", "DELETE"][pick(r, [80, 15, 5])];
  const status = [200, 304, 404, 500, 503][pick(r, [85, 5, 6, 3, 1])];
  const micros = r.next(2000000);
  const ms = `${Math.trunc(micros / 1000)}.${String(micros % 1000).padStart(3, "0")}`;
  const ip = client(r);
  const path = requestPath(r);
  return `${ip} - - [${time} +0000] "${method} ${path} HTTP/1.1" ${status} ${r.next(50000)} ${ms}`;
}

async function generate(path, count, seed) {
  const r = new Random(seed);
  const out = createWriteStream(path);
  let batch = [];
  for (let i = 0; i < count; i++) {
    batch.push(logLine(r, i, count));
    if (batch.length === 1000) {
      if (!out.write(batch.join("\n") + "\n")) await once(out, "drain");
      batch = [];
    }
  }
  if (batch.length > 0) out.write(batch.join("\n") + "\n");
  out.end();
  await once(out, "finish");
}

const LINE = /^(\S+) \S+ \S+ \[([^:\]]+:\d\d)[^\]]*\] "(\w+) (\S+) [^"]*" (\d{3}) (\d+) ([\d.]+)$/;

function parseLine(line) {
  const m = LINE.exec(line);
  if (!m) return null;
  const [, ip, hour, method, path, status, bytes, ms] = m;
  return { ip, hour, method, path, status: +status, bytes: +bytes, ms: +ms };
}

const routeOf = (path) =>
  path
    .split("?")[0]
    .split("/")
    .map((seg) => (/^\d+$/.test(seg) ? ":id" : seg))
    .join("/");

class Stats {
  lines = 0;
  malformed = 0;
  bytes = 0;
  statusClasses = new Map();
  routes = new Map();
  clients = new Map();
  hours = new Map();

  add(e, route) {
    this.bytes += e.bytes;
    const cls = `${Math.floor(e.status / 100)}xx`;
    this.statusClasses.set(cls, (this.statusClasses.get(cls) ?? 0) + 1);
    this.clients.set(e.ip, (this.clients.get(e.ip) ?? 0) + 1);
    const isError = e.status >= 500;
    const key = `${e.method} ${route}`;
    let r = this.routes.get(key);
    if (!r) {
      r = { count: 0, errors: 0, bytes: 0, latencies: [] };
      this.routes.set(key, r);
    }
    r.count++;
    r.bytes += e.bytes;
    r.latencies.push(e.ms);
    if (isError) r.errors++;
    let h = this.hours.get(e.hour);
    if (!h) {
      h = { count: 0, errors: 0 };
      this.hours.set(e.hour, h);
    }
    h.count++;
    if (isError) h.errors++;
  }
}

async function analyze(path) {
  const stats = new Stats();
  const rl = createInterface({ input: createReadStream(path), crlfDelay: Infinity });
  for await (const line of rl) {
    stats.lines++;
    const e = parseLine(line);
    if (!e) {
      stats.malformed++;
      continue;
    }
    stats.add(e, routeOf(e.path));
  }
  return stats;
}

// Same rounding as the Velt `fixed1` (toFixed rounds binary values differently at .x5).
function fixed1(x) {
  const tenths = Math.round(x * 10);
  return `${Math.trunc(tenths / 10)}.${tenths % 10}`;
}

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[rank < 1 ? 0 : rank - 1];
}

const percent = (part, whole) => (whole === 0 ? "0.0" : fixed1((part * 100) / whole));
const byCountDesc = (a, b) => (a[1] !== b[1] ? b[1] - a[1] : a[0] < b[0] ? -1 : 1);

function routeLine(name, r) {
  const sorted = [...r.latencies].sort((a, b) => a - b);
  const cols = [
    `${r.count}`.padStart(8),
    percent(r.errors, r.count).padStart(6),
    fixed1(percentile(sorted, 50)).padStart(8),
    fixed1(percentile(sorted, 95)).padStart(8),
    fixed1(percentile(sorted, 99)).padStart(8),
  ];
  return `${cols.join(" ")}  ${name}`;
}

function report(stats) {
  const parsed = stats.lines - stats.malformed;
  const out = [
    `lines: ${stats.lines}  parsed: ${parsed}  malformed: ${stats.malformed}  bytes: ${stats.bytes}`,
    `routes: ${stats.routes.size}  clients: ${stats.clients.size}  hours: ${stats.hours.size}`,
  ];
  const classes = [...stats.statusClasses.keys()].sort();
  out.push(`status: ${classes.map((c) => `${c} ${stats.statusClasses.get(c)} (${percent(stats.statusClasses.get(c), parsed)}%)`).join("  ")}`);
  const routes = [...stats.routes].map(([k, r]) => [k, r.count]).sort(byCountDesc);
  out.push("top 10 routes:", "   count   err%     p50      p95      p99  route (ms)");
  for (const [name] of routes.slice(0, 10)) out.push(routeLine(name, stats.routes.get(name)));
  out.push("top 5 clients:");
  for (const [ip, count] of [...stats.clients].sort(byCountDesc).slice(0, 5)) {
    out.push(`${`${count}`.padStart(8)}  ${ip}`);
  }
  out.push("top 3 hours by 5xx:");
  const hours = [...stats.hours].map(([k, h]) => [k, h.errors]).sort(byCountDesc);
  for (const [hour, errors] of hours.slice(0, 3)) out.push(`${`${errors}`.padStart(8)}  ${hour}:00`);
  return out;
}

const [command, file, lines = "1000000", seed = "42"] = process.argv.slice(2);
const start = performance.now();
if (command === "gen") {
  await generate(file, Number(lines), Number(seed));
} else if (command === "report") {
  for (const line of report(await analyze(file))) console.log(line);
} else {
  console.error("usage: pipeline.mjs gen|report <file> [lines] [seed]");
  process.exit(2);
}
if (process.env.TIMING) console.error(`elapsed: ${Math.round(performance.now() - start)} ms`);
