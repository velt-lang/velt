// Serves the registry Worker (packages/registry, built with `--host workers`) over HTTP under
// node, with in-memory R2 and D1 (fakes.mjs, the D1 tables made by its migrations), so the real
// `velt` client can publish to it and install from it (tests/registry_worker.rs). argv: the
// build's directory, the module's file name, the migrations directory. Prints
// `port <n> token <t>`: the port, and the token of user `alice`, made through the admin API.
import http from "node:http";
import { readdir, readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { Bucket, Database } from "./fakes.mjs";

const [dir, file, migrations] = process.argv.slice(2);
const { createWorker } = await import(pathToFileURL(`${dir}/velt_worker.mjs`).href);
const db = new Database();
for (const m of (await readdir(migrations)).filter((f) => f.endsWith(".sql")).sort()) {
  await db.exec(await readFile(`${migrations}/${m}`, "utf8"));
}
const env = { BLOBS: new Bucket(), DB: db, ADMIN_TOKEN: "admin-secret", REGISTRY_OPEN: "false" };
const worker = createWorker(new WebAssembly.Module(await readFile(`${dir}/${file}`)), {
  write: (stream, bytes) => process.stderr.write(bytes),
});
const ctx = { waitUntil() {} };

const server = http.createServer(async (req, res) => {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const method = req.method ?? "GET";
  const request = new Request(`http://127.0.0.1${req.url}`, {
    method,
    headers: req.headers,
    body: method === "GET" || method === "HEAD" ? undefined : Buffer.concat(chunks),
  });
  const r = await worker.fetch(request, env, ctx);
  res.writeHead(r.status, Object.fromEntries(r.headers));
  res.end(Buffer.from(await r.arrayBuffer()));
});
server.listen(0, "127.0.0.1", async () => {
  const admin = new Request("http://127.0.0.1/api/v1/_admin/users/alice", {
    method: "POST",
    headers: { authorization: "Bearer admin-secret" },
  });
  const token = (await (await worker.fetch(admin, env, ctx)).text()).trim();
  console.log(`port ${server.address().port} token ${token}`);
});
