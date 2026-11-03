// Runs a `velt build --host host-cloudflare` directory under workerd, the open-source Workers
// runtime, through Miniflare (crates/veltc/tests/workerd.rs):
// `node run.mjs <modules> <worker dir>`, where <modules> is the node_modules of
// tests/workerd/package.json (the pinned runtime). Every `.mjs` and `.wasm` file of the directory
// (and of its subdirectories) is a module of the Worker, as `wrangler deploy` uploads them; its
// bindings are workerd's local R2 bucket, D1 database, KV namespace, cache and a Durable
// Object namespace of a JavaScript Worker beside it; `fetch` of `https://upstream.test/` reaches a
// stub. Sends each path a request and prints the answers. The Worker's logs go to stderr.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [modules, dir] = process.argv.slice(2);
const miniflare = pathToFileURL(join(modules, "miniflare/dist/src/index.js")).href;
const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = await import(miniflare);

/** The compatibility date and flags of the `wrangler.toml` the build scaffolded. */
function compatibility() {
  const toml = readFileSync(join(dir, "wrangler.toml"), "utf8");
  const date = /^compatibility_date\s*=\s*"([^"]+)"/m.exec(toml);
  if (date === null) throw new Error(`no compatibility_date in ${join(dir, "wrangler.toml")}`);
  const list = /^compatibility_flags\s*=\s*\[([^\]]*)\]/m.exec(toml)?.[1] ?? "";
  const flags = [...list.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  return { compatibilityDate: date[1], compatibilityFlags: flags };
}

/** The directory's `.mjs` and `.wasm` files, its subdirectories' too (`hosts/<package>/…`). */
function walk(at) {
  return readdirSync(at, { withFileTypes: true }).flatMap((e) => {
    const path = join(at, e.name);
    return e.isDirectory() ? walk(path) : /\.(mjs|wasm)$/.test(e.name) ? [path] : [];
  });
}
// worker.mjs, the entry, first.
const entry = join(dir, "worker.mjs");
const files = [entry, ...walk(dir).filter((f) => f !== entry).sort()];
const workerModules = files.map((path) => ({
  type: path.endsWith(".wasm") ? "CompiledWasm" : "ESModule",
  path,
}));

const COUNTER = `
export class Counter {
  constructor(state) { this.state = state; }
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/count") {
      const n = ((await this.state.storage.get("n")) ?? 0) + 1;
      await this.state.storage.put("n", n);
      return new Response(String(n));
    }
    return new Response(request.method + " " + (await request.text()), { status: 201 });
  }
}
export default { fetch() { return new Response("counter"); } };
`;

/** Miniflare's own messages, on stderr: stdout is what the test compares. */
class StderrLog extends Log {
  log(message) {
    process.stderr.write(`${message}\n`);
  }
}

// The options in Miniflare 4's shape, which Miniflare 5 converts. `cf: false`: no download of
// Cloudflare's `request.cf` data at start-up (the test needs no network).
const mf = new Miniflare(convertV4MiniflareOptions({
  log: new StderrLog(LogLevel.WARN),
  cf: false,
  handleStructuredLogs: (log) => process.stderr.write(`${log.level}: ${log.message}\n`),
  workers: [
    {
      name: "app",
      ...compatibility(),
      modulesRoot: dir,
      modules: workerModules,
      r2Buckets: ["BUCKET"],
      d1Databases: ["DB"],
      kvNamespaces: ["KV"],
      durableObjects: { COUNTER: { className: "Counter", scriptName: "counter" } },
      outboundService(request) {
        const url = new URL(request.url);
        if (url.hostname !== "upstream.test") return new Response("no network", { status: 502 });
        return new Response(`data at ${url.pathname}`, { headers: { "x-upstream": "1" } });
      },
    },
    // The worker that defines the class declares its namespace too.
    {
      name: "counter",
      modules: true,
      script: COUNTER,
      compatibilityDate: "2026-08-01",
      durableObjects: { COUNTER: "Counter" },
    },
  ],
}));

/** Sends `path` a request, giving up after 30 seconds: a Worker that never answers fails. */
function dispatch(path, init) {
  const signal = AbortSignal.timeout(30_000);
  return mf.dispatchFetch(`https://w.test${path}`, { ...init, signal });
}

/** Sends `path` a request and prints the answer. */
async function send(path, init) {
  const res = await dispatch(path, init);
  const type = res.headers.get("content-type") ?? "none";
  console.log(`${init?.method ?? "GET"} ${path}: ${res.status} ${type}\n${await res.text()}`);
}

try {
  await send("/hello");
  await send("/json", { method: "POST", body: JSON.stringify({ name: "Ada", times: 21 }) });
  await send("/echo", { method: "PUT", body: "x".repeat(100_000) });
  await send("/stream");
  await send("/throw");
  await send("/missing");
  await send("/upstream");
  // Requests at once, each in a context of its own.
  const many = await Promise.all(
    Array.from({ length: 8 }, (_, i) => dispatch("/echo", { method: "POST", body: "y".repeat(i) })),
  );
  console.log(`concurrent: ${(await Promise.all(many.map((r) => r.text()))).join(", ")}`);
  for (const path of ["/r2", "/d1", "/kv", "/cache", "/durable"]) await send(path);
} finally {
  await mf.dispose();
}
