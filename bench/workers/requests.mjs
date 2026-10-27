// Time per request of a Worker built with `velt build --release --host workers`, under node
// (docs/internals/design/cloudflare-workers.md §7): the first request of an isolate (compiling
// and instantiating the module, `main`, the request), then the mean of `n` more.
//
//   velt build --release --host workers crates/veltc/tests/workers/serve.vlt -o /tmp/w/serve
//   node bench/workers/requests.mjs /tmp/w serve.wasm "/json?q=x" 20000
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
const [dir, file, path, n] = process.argv.slice(2);
const { createWorker } = await import(pathToFileURL(`${dir}/velt_worker.mjs`).href);
const t0 = performance.now();
const module = new WebAssembly.Module(await readFile(`${dir}/${file}`));
const worker = createWorker(module, { write() {} });
const first = await worker.fetch(new Request(`https://e.com${path}`), {}, {});
await first.text();
const t1 = performance.now();
for (let i = 0; i < Number(n); i++) {
  const r = await worker.fetch(new Request(`https://e.com${path}`), {}, {});
  await r.text();
}
const t2 = performance.now();
console.log(`compile+instantiate+main+first request: ${(t1 - t0).toFixed(1)} ms; then ${((t2 - t1) / n * 1000).toFixed(1)} µs per request`);
