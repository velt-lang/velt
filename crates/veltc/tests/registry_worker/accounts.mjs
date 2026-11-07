// What signing in can't be talked out of (crates/veltc/tests/registry_worker.rs): a Worker without
// its GitHub secret refuses to start a sign-in, and a user who owns packages can't be deleted, so
// their name never goes to another account with the packages. `node accounts.mjs <modules> <worker
// dir>`, where <modules> is tests/workerd's node_modules. Exits 1 on a failure.

import assert from "node:assert/strict";
import { start } from "./start.mjs";

const [modules, dir] = process.argv.slice(2);
const { mf, url } = await start(modules, dir, {
  PUBLIC_URL: "https://registry.test",
  GITHUB_CLIENT_ID: "the-client",
});
try {
  // No secret: no sign-in starts, and nothing goes to GitHub.
  const res = await fetch(new URL("/login", url), { redirect: "manual" });
  assert.equal(res.status, 500);
  assert.equal(res.headers.get("location"), null);
  assert.equal(res.headers.get("set-cookie"), null);

  const db = await mf.getD1Database("DB");
  await db.prepare("INSERT INTO users (name, github_id) VALUES ('vic', 301)").run();
  await db.prepare("INSERT INTO owners (package, position, user) VALUES ('vicpkg', 0, 'vic')").run();
  await assert.rejects(db.prepare("DELETE FROM users WHERE name = 'vic'").run(), /FOREIGN KEY/);
  await assert.rejects(
    db.prepare("INSERT INTO owners (package, position, user) VALUES ('x', 0, 'ghost')").run(),
    /FOREIGN KEY/,
  );
  console.log("ok");
} finally {
  await mf.dispose();
}
