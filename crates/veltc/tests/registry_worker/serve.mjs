// Serves the registry's Worker under workerd with its real bindings: workerd's local R2, D1 and
// Durable Objects (crates/veltc/tests/registry_worker.rs). `node serve.mjs <modules> <worker dir>
// <purge log> <user>=<token>…`, where <modules> is tests/workerd's node_modules. Adds the users by
// the hashes of their tokens, as an administrator does, and prints the URL. Runs until stdin ends. A
// zone is configured, and each purge the Worker sends Cloudflare's API goes to the log, as JSON.

import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import { start } from "./start.mjs";

const [modules, dir, purges, ...users] = process.argv.slice(2);
const vars = {
  CACHE_ZONE_ID: "the-zone",
  CACHE_PURGE_TOKEN: "the-purge-token",
  PUBLIC_URL: "https://registry.test",
};
async function outbound(request) {
  const line = {
    url: request.url,
    method: request.method,
    authorization: request.headers.get("authorization"),
    body: await request.json(),
  };
  appendFileSync(purges, `${JSON.stringify(line)}\n`);
  return new Response("{}");
}
const { mf, url } = await start(modules, dir, vars, outbound);

const db = await mf.getD1Database("DB");
for (const user of users) {
  const [name, token] = user.split("=");
  const hash = `sha256:${createHash("sha256").update(token).digest("hex")}`;
  await db.prepare("INSERT INTO users (name, token_hash) VALUES (?, ?)").bind(name, hash).run();
}

console.log(url.href);
// Until stdin closes, which also happens when the test is killed: stopping Miniflare stops workerd,
// which a killed process would leave running.
process.stdin.resume();
process.stdin.on("end", async () => {
  await mf.dispose();
  process.exit(0);
});
