// Drives a `velt build --host workers` output (argv[2]: its directory, argv[3]: the module's
// file name) the way a Worker runs it: requests in, responses out. Prints each response and
// the program's output.
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const [dir, file] = process.argv.slice(2);
const { createWorker } = await import(pathToFileURL(`${dir}/velt_worker.mjs`).href);
const decoder = new TextDecoder();
const worker = createWorker(new WebAssembly.Module(await readFile(`${dir}/${file}`)), {
  write: (stream, bytes) => process.stdout.write(`[${stream}] ${decoder.decode(bytes)}`),
});
const requests = [
  new Request("https://example.com/json?q=velt"),
  new Request("https://example.com/greet", {
    method: "POST",
    headers: { "user-agent": "drive" },
    body: JSON.stringify({ name: "Workers" }),
  }),
  new Request("https://example.com/bytes"),
  new Request("https://example.com/fail"),
  new Request("https://example.com/missing"),
];
for (const request of requests) {
  const res = await worker.fetch(request, {}, { waitUntil() {} });
  const headers = [...res.headers].map(([k, v]) => `${k}=${v}`).join(" ");
  console.log(`${res.status} ${headers} | ${await res.text()}`);
}
