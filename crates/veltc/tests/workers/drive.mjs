// Drives a `velt build --host workers` output the way a Worker runs it: requests in, responses
// out. argv: the build's directory, the module's file name, and the scenario (`<name>.mjs` next
// to the program: `requests`, a list of `Request`s, and optionally `fetch`, the upstream the
// program's own requests go to). Prints each response and the program's output.
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const [dir, file, scenario] = process.argv.slice(2);
const { createWorker } = await import(pathToFileURL(`${dir}/velt_worker.mjs`).href);
const { requests, fetch } = await import(pathToFileURL(scenario).href);
const decoder = new TextDecoder();
const worker = createWorker(new WebAssembly.Module(await readFile(`${dir}/${file}`)), {
  write: (stream, bytes) => process.stdout.write(`[${stream}] ${decoder.decode(bytes)}`),
  fetch: fetch ?? (() => Promise.reject(new Error("no network in this test"))),
});
for (const request of requests()) {
  const res = await worker.fetch(request, {}, { waitUntil() {} });
  const headers = [...res.headers].map(([k, v]) => `${k}=${v}`).join(" ");
  console.log(`${res.status} ${headers} | ${await res.text()}`);
}
