// Drives a `velt build --host host-cloudflare` directory of a program using packages/cloudflare
// (crates/veltc/tests/cloudflare.rs): `node drive.mjs <worker dir>` imports the directory's
// `worker.mjs` as Workers do, with fakes.mjs's bindings as its `env` and a fake `caches`, sends
// it one request per binding and prints each answer. Each request runs in a context of its own
// (an `AsyncLocalStorage` store), and what Workers forbid is printed (`context: …`): an R2 body
// read from another request's context. The table of `HostRef`s is watched, so the test sees the
// program release the R2 objects it drops.

import { AsyncLocalStorage } from "node:async_hooks";
import * as nodeModule from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { FakeR2ObjectBody, fakeCaches, fakeEnv } from "./fakes.mjs";

// `import module from "./program.wasm"` gives the compiled module, as on Workers.
const wasmSource = (url) =>
  "import { readFileSync } from 'node:fs';\n" +
  `export default new WebAssembly.Module(readFileSync(new URL(${JSON.stringify(url)})));`;
if (typeof nodeModule.registerHooks === "function") {
  nodeModule.registerHooks({
    load(url, context, next) {
      if (!url.endsWith(".wasm")) return next(url, context);
      return { format: "module", shortCircuit: true, source: wasmSource(url) };
    },
  });
} else {
  const hooks = `export async function load(url, context, next) {
    if (!url.endsWith(".wasm")) return next(url, context);
    return { format: "module", shortCircuit: true, source: (${wasmSource})(url) };
  }`;
  nodeModule.register(`data:text/javascript,${encodeURIComponent(hooks)}`);
}

console.error = (...args) => console.log("log:", ...args);
const contexts = new AsyncLocalStorage();
const watch = {
  context: () => contexts.getStore(),
  report: (what) => console.log(`context: ${what}`),
};

const [dir] = process.argv.slice(2);
// The glue's table of `HostRef`s (velt_web.mjs's `HostRefs`, which the core's table extends).
const { HostRefs } = await import(pathToFileURL(join(dir, "velt_web.mjs")).href);
let table = null;
const add = HostRefs.prototype.add;
HostRefs.prototype.add = function (value) {
  table = this;
  return add.call(this, value);
};
/** How many R2 objects with a body the program holds. */
const heldObjects = () =>
  table === null ? 0 : [...table.values.values()].filter((v) => v instanceof FakeR2ObjectBody).length;

globalThis.caches = fakeCaches();
const env = fakeEnv(watch);
await env.BUCKET.put("big", new Uint8Array(10_000).fill(7));
const worker = (await import(pathToFileURL(join(dir, "worker.mjs")).href)).default;

let requests = 0;
async function send(path) {
  const id = ++requests;
  const waited = [];
  const ctx = { waitUntil: (p) => waited.push(p), passThroughOnException() {} };
  const res = await contexts.run(id, () => worker.fetch(new Request(`https://w.test/${path}`), env, ctx));
  console.log(`${path}: ${res.status}\n${await res.text()}`);
  await Promise.all(waited);
}

for (const path of ["r2", "d1", "kv", "cache", "durable", "failures"]) await send(path);
await send("refs");
console.log(`R2 objects held after dropping them: ${heldObjects()}`);
await send("hold");
console.log(`R2 objects held while kept: ${heldObjects()}`);
await send("release");
console.log(`R2 objects held after releasing it: ${heldObjects()}`);
