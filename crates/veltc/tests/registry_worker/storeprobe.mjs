// registry-cloudflare's stores refuse a stale write (crates/veltc/tests/registry_worker.rs), driven
// through store-probe. D1MetadataDb's commit made from a revision that is no longer the package's
// changes nothing: a losing writer sends exactly that, so no interleaving is needed, and the rows
// are read back here. R2BlobStore's write over a tag it read loses once the same bytes were written
// again (R2's etag is the same; the upload time isn't). `node storeprobe.mjs <modules>
// <store-probe's Worker dir>`. Exits 1 on a failure.

import assert from "node:assert/strict";
import { start } from "./start.mjs";

const [modules, dir] = process.argv.slice(2);
const { mf, url } = await start(modules, dir);
try {
  const db = await mf.getD1Database("DB");
  for (const name of ["alice", "bob"]) {
    await db.prepare("INSERT INTO users (name) VALUES (?)").bind(name).run();
  }
  /** Whether the commit of `name` `version` (checksum `sum`) from `base` won. */
  async function commit(name, base, version, sum, owner = "alice") {
    const body = JSON.stringify({ name, base, version, checksum: sum, owner });
    const res = await fetch(new URL("/commit", url), { method: "POST", body });
    assert.equal(res.status, 200);
    return (await res.text()) === "true";
  }
  const rows = async (sql, ...values) => (await db.prepare(sql).bind(...values).all()).results;

  assert.equal(await commit("my-lib", 0, "1.0.0", "sha256:aa"), true);
  // Losers: the same name new again, the other spelling new, and a write from revision 1 once the
  // package moved to 2.
  assert.equal(await commit("my-lib", 0, "1.0.0", "sha256:bb", "bob"), false);
  assert.equal(await commit("my_lib", 0, "1.0.0", "sha256:cc", "bob"), false);
  assert.equal(await commit("my-lib", 1, "1.1.0", "sha256:dd"), true);
  assert.equal(await commit("my-lib", 1, "1.0.0", "sha256:ee"), false);
  assert.equal(await commit("my-lib", 1, "1.2.0", "sha256:ff"), false);

  // Only the winners' rows.
  assert.deepEqual(await rows("SELECT name, revision FROM packages"), [
    { name: "my-lib", revision: 2 },
  ]);
  const versions = await rows("SELECT package, version, entry_json FROM versions ORDER BY position");
  assert.deepEqual(versions.map((v) => `${v.package} ${v.version}`), ["my-lib 1.0.0", "my-lib 1.1.0"]);
  assert.ok(versions[0].entry_json.includes("sha256:aa"), versions[0].entry_json);
  assert.deepEqual(await rows("SELECT package, user FROM owners"), [
    { package: "my-lib", user: "alice" },
  ]);
  assert.deepEqual((await rows("SELECT package FROM search")).map((r) => r.package), ["my-lib"]);

  // R2: the first write and the same bytes again win; the write over the first tag loses, and the
  // second file (revision 2) stays.
  const aba = await fetch(new URL("/aba", url), { method: "POST" });
  assert.equal(await aba.text(), "true true false 2");
  console.log("ok");
} finally {
  await mf.dispose();
}
