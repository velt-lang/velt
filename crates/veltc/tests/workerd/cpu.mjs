// Times a `velt build --host host-cloudflare` directory under node
// (crates/veltc/tests/workerd.rs): `node cpu.mjs <worker dir> <requests>` imports its `worker.mjs`
// as Workers do (the `.wasm` import is the compiled module) and prints one JSON object: `startup`,
// the CPU milliseconds (process.cpuUsage, user and system, all of node's threads) from importing it
// (compiling its module) to the first request's answer; and for `/hello` (text) and `/json` (a JSON body parsed and another one answered), the
// CPU milliseconds per request (process.cpuUsage, user and system) at the median and the 90th
// percentile. V8 runs the Worker as in workerd, but not workerd's V8 or its `env` and `ctx`, so
// these are guards against regressions, compared with Cloudflare's limits (10 ms of CPU per
// request on the Free plan, 1 second to start).

import * as nodeModule from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const wasmSource = (url) =>
  "import { readFileSync } from 'node:fs';\n" +
  `export default new WebAssembly.Module(readFileSync(new URL(${JSON.stringify(url)})));`;
nodeModule.registerHooks({
  load(url, context, next) {
    if (!url.endsWith(".wasm")) return next(url, context);
    return { format: "module", shortCircuit: true, source: wasmSource(url) };
  },
});

const [dir, count] = process.argv.slice(2);
const requests = Number(count);
const ctx = { waitUntil() {}, passThroughOnException() {} };
const JSON_BODY = JSON.stringify({ name: "Ada", times: 21 });

const started = process.cpuUsage();
const worker = (await import(pathToFileURL(join(dir, "worker.mjs")).href)).default;

/** Sends `path` a request and reads the answer; throws unless it is a 200. */
async function send(path) {
  const init = path === "/json" ? { method: "POST", body: JSON_BODY } : undefined;
  const res = await worker.fetch(new Request(`https://w.test${path}`, init), {}, ctx);
  await res.arrayBuffer();
  if (res.status !== 200) throw new Error(`${path}: ${res.status}`);
}

await send("/hello");
const used = process.cpuUsage(started);
const startup = (used.user + used.system) / 1000;

/** CPU milliseconds per request of `path`: the median and the 90th percentile. */
async function cpu(path) {
  // Warm, as a Worker serving traffic is.
  for (let i = 0; i < 20; i++) await send(path);
  const times = [];
  for (let i = 0; i < requests; i++) {
    const before = process.cpuUsage();
    await send(path);
    const used = process.cpuUsage(before);
    times.push((used.user + used.system) / 1000);
  }
  times.sort((a, b) => a - b);
  const at = (q) => Number(times[Math.floor(q * (times.length - 1))].toFixed(3));
  return { p50: at(0.5), p90: at(0.9) };
}

const hello = await cpu("/hello");
const json = await cpu("/json");
console.log(JSON.stringify({ startup: Number(startup.toFixed(1)), hello, json }));
