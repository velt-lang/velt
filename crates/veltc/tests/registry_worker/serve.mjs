// Serves the registry's Worker under workerd with its real bindings: workerd's local R2, D1 and
// Durable Objects (crates/veltc/tests/registry_worker.rs). `node serve.mjs <modules> <worker dir>
// <purge log> <user>=<token>…`, where <modules> is tests/workerd's node_modules. Adds the users by
// the hashes of their tokens, as an administrator does, and prints the URL. Runs until stdin ends. A
// zone is configured, and each purge the Worker sends Cloudflare's API goes to the log, as JSON.
// GitHub is a stub: an OAuth app `the-client`, whose codes `code-<x>` sign in the accounts of
// `ACCOUNTS` (registry_worker/login.mjs).

import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import { start } from "./start.mjs";

const [modules, dir, purges, ...users] = process.argv.slice(2);
const vars = {
  CACHE_ZONE_ID: "the-zone",
  CACHE_PURGE_TOKEN: "the-purge-token",
  PUBLIC_URL: "https://registry.test",
  GITHUB_CLIENT_ID: "the-client",
  GITHUB_CLIENT_SECRET: "the-secret",
};

/** The GitHub accounts each code signs in. */
const ACCOUNTS = {
  "code-carol": { id: 101, login: "Carol" },
  "code-carol-renamed": { id: 101, login: "Caroline" },
  "code-digit": { id: 102, login: "9lives" },
  "code-alice": { id: 103, login: "alice" },
  "code-angle": { id: 104, login: "<b>x" },
};

/** GitHub's answer to `request`, or null when it isn't GitHub's. */
async function github(request) {
  const url = new URL(request.url);
  // Anything the Worker sends to `evil.test` would sign carol in: it must never get there.
  if (url.hostname === "evil.test") return Response.json({ access_token: "gho-code-carol" });
  if (url.href === "https://github.com/login/oauth/access_token") {
    const { client_id, client_secret, code, redirect_uri } = await request.json();
    if (code === "code-redirect") {
      return new Response(null, { status: 307, headers: { location: "https://evil.test/steal" } });
    }
    const ok =
      client_id === "the-client" &&
      client_secret === "the-secret" &&
      redirect_uri === "https://registry.test/login/callback" &&
      code in ACCOUNTS;
    return Response.json(ok ? { access_token: `gho-${code}` } : { error: "bad_verification_code" });
  }
  if (url.href === "https://api.github.com/user") {
    const code = (request.headers.get("authorization") ?? "").replace(/^Bearer gho-/, "");
    return code in ACCOUNTS ? Response.json(ACCOUNTS[code]) : new Response("", { status: 401 });
  }
  return null;
}

async function outbound(request) {
  const answer = await github(request);
  if (answer !== null) return answer;
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
  await db.prepare("INSERT INTO users (name) VALUES (?)").bind(name).run();
  await db
    .prepare("INSERT INTO tokens (token_hash, user, created) VALUES (?, ?, 0)")
    .bind(hash, name)
    .run();
}

console.log(url.href);
// Until stdin closes, which also happens when the test is killed: stopping Miniflare stops workerd,
// which a killed process would leave running.
process.stdin.resume();
process.stdin.on("end", async () => {
  await mf.dispose();
  process.exit(0);
});
